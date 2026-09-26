//! CSS: parsing style sheets, selectors and matching them against the DOM.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::dom::{Dom, NodeData, NodeId};

#[derive(Clone, Debug)]
pub struct Decl {
    pub name: String,
    pub value: String,
    pub important: bool,
}

pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Decl>,
}

#[derive(Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// Addresses from @import, for the loader to fetch.
    pub imports: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Combinator {
    Descendant,
    Child,
    Next,
    Later,
}

#[derive(Clone, Debug)]
pub enum AttrOp {
    Exists,
    Equals(String),
    Word(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
    Dash(String),
}

#[derive(Clone, Debug)]
pub enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    FirstOfType,
    LastOfType,
    Root,
    Link,
    Empty,
    Checked,
    Not(Box<Selector>),
    Is(Vec<Selector>),
    /// :hover, :focus, ::before and the like: never matches.
    Never,
    /// Harmless ones we treat as always true (:focus-within is not).
    Always,
}

#[derive(Clone, Debug, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, AttrOp)>,
    pub pseudo: Vec<Pseudo>,
}

/// A complex selector, stored from the subject (rightmost) outwards.
#[derive(Clone, Debug)]
pub struct Selector {
    pub parts: Vec<(Compound, Combinator)>,
    pub specificity: u32,
}

/// What @media queries are asked about.
pub struct Media {
    pub width: i32,
    pub height: i32,
}

// ---- parsing ------------------------------------------------------------------

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("/*") {
        out.push_str(&rest[..pos]);
        match rest[pos + 2..].find("*/") {
            Some(e) => rest = &rest[pos + 2 + e + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Index of the `}` that closes a block whose `{` is just before `s`.
fn block_end(s: &str) -> usize {
    let mut depth = 1;
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            },
        }
    }
    s.len()
}

pub fn parse_stylesheet(source: &str, media: &Media) -> Stylesheet {
    let text = strip_comments(source);
    let mut sheet = Stylesheet::default();
    parse_rules(&text, media, &mut sheet, 0);
    sheet
}

fn parse_rules(mut s: &str, media: &Media, sheet: &mut Stylesheet, depth: u32) {
    loop {
        s = s.trim_start();
        if s.is_empty() || depth > 8 {
            return;
        }
        if s.starts_with('}') {
            s = &s[1..];
            continue;
        }
        if let Some(rest) = s.strip_prefix('@') {
            // at-rule: either `@x ...;` or `@x ... { ... }`
            let semi = rest.find(';');
            let brace = rest.find('{');
            match (semi, brace) {
                (Some(sc), b) if b.is_none_or(|b| sc < b) => {
                    let stmt = &rest[..sc];
                    if let Some(url) = stmt.strip_prefix("import") {
                        if let Some(u) = parse_url(url.trim()) {
                            sheet.imports.push(u);
                        }
                    }
                    s = &rest[sc + 1..];
                }
                (_, Some(b)) => {
                    let prelude = rest[..b].trim();
                    let body_start = b + 1;
                    let end = body_start + block_end(&rest[body_start..]);
                    let body = &rest[body_start..end.min(rest.len())];
                    let lower = prelude.to_ascii_lowercase();
                    if let Some(q) = lower.strip_prefix("media") {
                        if media_matches(q, media) {
                            parse_rules(body, media, sheet, depth + 1);
                        }
                    } else if lower.starts_with("supports")
                        || lower.starts_with("layer")
                        || lower.starts_with("container")
                        || lower.starts_with("document")
                    {
                        parse_rules(body, media, sheet, depth + 1);
                    }
                    s = rest.get(end + 1..).unwrap_or("");
                }
                _ => return,
            }
            continue;
        }
        let Some(b) = s.find('{') else {
            return;
        };
        let prelude = &s[..b];
        let body_start = b + 1;
        let end = body_start + block_end(&s[body_start..]);
        let body = &s[body_start..end.min(s.len())];
        let selectors: Vec<Selector> = split_top(prelude, ',')
            .iter()
            .filter_map(|p| parse_selector(p))
            .collect();
        if !selectors.is_empty() {
            let decls = parse_declarations(body);
            if !decls.is_empty() {
                sheet.rules.push(Rule { selectors, decls });
            }
        }
        s = s.get(end + 1..).unwrap_or("");
    }
}

pub fn parse_url(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = if let Some(r) = s.strip_prefix("url(") {
        r.split(')').next()?.trim()
    } else {
        s.split_whitespace().next()?
    };
    let inner = inner.trim_matches(|c| c == '"' || c == '\'');
    (!inner.is_empty()).then(|| inner.to_string())
}

/// Evaluate a media query (for matchMedia).
pub fn media_query(query: &str, m: &Media) -> bool {
    media_matches(&query.to_ascii_lowercase(), m)
}

fn media_matches(query: &str, m: &Media) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    split_top(query, ',').iter().any(|q| {
        let q = q.trim();
        let (negate, q) = match q.strip_prefix("not ") {
            Some(r) => (true, r),
            None => (false, q),
        };
        let q = q.strip_prefix("only ").unwrap_or(q);
        let mut ok = true;
        for part in q.split(" and ") {
            let part = part.trim();
            let result = if part.starts_with('(') {
                feature_matches(part.trim_matches(|c| c == '(' || c == ')'), m)
            } else {
                matches!(part, "screen" | "all" | "")
            };
            ok &= result;
        }
        ok != negate
    })
}

fn feature_matches(f: &str, m: &Media) -> bool {
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => {
            // range syntax like (width >= 600px)
            for (op, cmp) in [(">=", 0), ("<=", 1), (">", 2), ("<", 3)] {
                if let Some((n, v)) = f.split_once(op) {
                    let n = n.trim();
                    let px = media_px(v.trim());
                    let actual = if n.contains("height") {
                        m.height
                    } else {
                        m.width
                    };
                    return match cmp {
                        0 => actual >= px,
                        1 => actual <= px,
                        2 => actual > px,
                        _ => actual < px,
                    };
                }
            }
            return matches!(f.trim(), "color" | "hover" | "pointer");
        }
    };
    match name {
        "min-width" => m.width >= media_px(value),
        "max-width" => m.width <= media_px(value),
        "min-height" => m.height >= media_px(value),
        "max-height" => m.height <= media_px(value),
        "orientation" => value == "landscape",
        "prefers-color-scheme" => value == "light",
        "prefers-reduced-motion" => value == "no-preference",
        "hover" | "any-hover" => value == "hover",
        "pointer" | "any-pointer" => value == "fine",
        "min-resolution" | "min-device-pixel-ratio" | "-webkit-min-device-pixel-ratio" => {
            value.starts_with('1') || value.starts_with("96")
        }
        _ => false,
    }
}

fn media_px(v: &str) -> i32 {
    let num: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let n = parse_f32(&num).unwrap_or(0.0);
    if v.ends_with("em") {
        (n * 16.0) as i32
    } else {
        n as i32
    }
}

pub fn parse_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut int = 0f32;
    let mut frac = 0f32;
    let mut scale = 1f32;
    let mut seen_dot = false;
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            any = true;
            let d = (c as u8 - b'0') as f32;
            if seen_dot {
                scale /= 10.0;
                frac += d * scale;
            } else {
                int = int * 10.0 + d;
            }
        } else if c == '.' && !seen_dot {
            seen_dot = true;
        } else if c == 'e' || c == 'E' {
            chars.next();
            let exp: String = chars.collect();
            let e: i32 = exp.parse().ok()?;
            let mut v = int + frac;
            for _ in 0..e.unsigned_abs().min(40) {
                if e > 0 {
                    v *= 10.0
                } else {
                    v /= 10.0
                }
            }
            return Some(if neg { -v } else { v });
        } else {
            return None;
        }
        chars.next();
    }
    any.then(|| if neg { -(int + frac) } else { int + frac })
}

/// Split on `sep` outside brackets and quotes.
pub fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut quote = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                c if c == sep && depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    out.push(&s[start..]);
    out
}

pub fn parse_declarations(body: &str) -> Vec<Decl> {
    let mut out = Vec::new();
    for part in split_top(body, ';') {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let name = if name.starts_with("--") {
            name.to_string()
        } else {
            name.to_ascii_lowercase()
        };
        let mut value = value.trim();
        let mut important = false;
        if let Some(pos) = value.to_ascii_lowercase().rfind("!important") {
            important = true;
            value = value[..pos].trim_end();
        }
        out.push(Decl {
            name,
            value: value.to_string(),
            important,
        });
    }
    out
}

pub fn parse_selector(text: &str) -> Option<Selector> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut compounds: Vec<(Compound, Combinator)> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut pending = Combinator::Descendant;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '>' => {
                pending = Combinator::Child;
                i += 1;
                continue;
            }
            '+' => {
                pending = Combinator::Next;
                i += 1;
                continue;
            }
            '~' => {
                pending = Combinator::Later;
                i += 1;
                continue;
            }
            _ => {}
        }
        let (compound, next) = parse_compound(&chars, i)?;
        if next == i {
            return None;
        }
        i = next;
        compounds.push((compound, pending));
        pending = Combinator::Descendant;
    }
    if compounds.is_empty() {
        return None;
    }
    // Store right to left. Each compound keeps the combinator that links it
    // to the compound on its left.
    let parts: Vec<(Compound, Combinator)> = compounds.into_iter().rev().collect();
    let mut spec = 0;
    for (c, _) in &parts {
        spec += specificity(c);
    }
    Some(Selector {
        parts,
        specificity: spec,
    })
}

fn specificity(c: &Compound) -> u32 {
    let mut s = 0;
    if c.id.is_some() {
        s += 1 << 16;
    }
    s += (c.classes.len() + c.attrs.len()) as u32 * (1 << 8);
    for p in &c.pseudo {
        s += match p {
            Pseudo::Not(inner) => inner.specificity,
            Pseudo::Is(list) => list.iter().map(|s| s.specificity).max().unwrap_or(0),
            _ => 1 << 8,
        };
    }
    if c.tag.is_some() {
        s += 1;
    }
    s
}

fn ident(chars: &[char], mut i: usize) -> (String, usize) {
    let mut out = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
            out.push(c);
            i += 1;
        } else {
            break;
        }
    }
    (out, i)
}

fn parse_compound(chars: &[char], mut i: usize) -> Option<(Compound, usize)> {
    let mut c = Compound::default();
    let start = i;
    if i < chars.len() && chars[i] == '*' {
        i += 1;
    } else if i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '_') {
        let (name, n) = ident(chars, i);
        c.tag = Some(name.to_ascii_lowercase());
        i = n;
    }
    while i < chars.len() {
        match chars[i] {
            '#' => {
                let (name, n) = ident(chars, i + 1);
                c.id = Some(name);
                i = n;
            }
            '.' => {
                let (name, n) = ident(chars, i + 1);
                if name.is_empty() {
                    return None;
                }
                c.classes.push(name);
                i = n;
            }
            '[' => {
                let end = chars[i..].iter().position(|&x| x == ']')? + i;
                let inner: String = chars[i + 1..end].iter().collect();
                c.attrs.push(parse_attr_sel(&inner)?);
                i = end + 1;
            }
            ':' => {
                let double = chars.get(i + 1) == Some(&':');
                let (name, mut n) = ident(chars, i + if double { 2 } else { 1 });
                let name = name.to_ascii_lowercase();
                let mut arg = String::new();
                if chars.get(n) == Some(&'(') {
                    let mut depth = 0;
                    let mut j = n;
                    while j < chars.len() {
                        match chars[j] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    arg = chars[n + 1..j.min(chars.len())].iter().collect();
                    n = (j + 1).min(chars.len());
                }
                let p = if double {
                    Pseudo::Never
                } else {
                    match name.as_str() {
                        "first-child" => Pseudo::FirstChild,
                        "last-child" => Pseudo::LastChild,
                        "only-child" => Pseudo::OnlyChild,
                        "first-of-type" => Pseudo::FirstOfType,
                        "last-of-type" => Pseudo::LastOfType,
                        "nth-child" => {
                            let (a, b) = parse_nth(&arg)?;
                            Pseudo::NthChild(a, b)
                        }
                        "nth-last-child" => {
                            let (a, b) = parse_nth(&arg)?;
                            Pseudo::NthLastChild(a, b)
                        }
                        "root" => Pseudo::Root,
                        "link" | "any-link" => Pseudo::Link,
                        "empty" => Pseudo::Empty,
                        "checked" => Pseudo::Checked,
                        "not" => {
                            let inner = split_top(&arg, ',');
                            if inner.len() == 1 {
                                Pseudo::Not(Box::new(parse_selector(inner[0])?))
                            } else {
                                Pseudo::Not(Box::new(Selector {
                                    parts: alloc::vec![(
                                        Compound {
                                            pseudo: alloc::vec![Pseudo::Is(
                                                inner
                                                    .iter()
                                                    .filter_map(|s| parse_selector(s))
                                                    .collect()
                                            )],
                                            ..Compound::default()
                                        },
                                        Combinator::Descendant
                                    )],
                                    specificity: 0,
                                }))
                            }
                        }
                        "is" | "where" | "matches" | "-webkit-any" => Pseudo::Is(
                            split_top(&arg, ',')
                                .iter()
                                .filter_map(|s| parse_selector(s))
                                .collect(),
                        ),
                        "visited" | "hover" | "active" | "focus" | "focus-visible"
                        | "focus-within" | "target" | "disabled" | "placeholder-shown"
                        | "invalid" | "has" | "fullscreen" | "indeterminate" => Pseudo::Never,
                        "enabled" | "lang" | "dir" | "defined" | "valid" | "scope" => {
                            Pseudo::Always
                        }
                        _ => Pseudo::Never,
                    }
                };
                c.pseudo.push(p);
                i = n;
            }
            _ => break,
        }
    }
    (i > start).then_some((c, i))
}

fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let a = arg.trim().to_ascii_lowercase();
    let a = a.split(" of ").next().unwrap_or("").replace(' ', "");
    match a.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(pos) = a.find('n') {
        let coef = &a[..pos];
        let a_val = match coef {
            "" | "+" => 1,
            "-" => -1,
            c => c.parse().ok()?,
        };
        let rest = &a[pos + 1..];
        let b = if rest.is_empty() {
            0
        } else {
            rest.parse().ok()?
        };
        Some((a_val, b))
    } else {
        Some((0, a.parse().ok()?))
    }
}

fn parse_attr_sel(inner: &str) -> Option<(String, AttrOp)> {
    let ops = ["~=", "^=", "$=", "*=", "|=", "="];
    for op in ops {
        if let Some((n, v)) = inner.split_once(op) {
            let name = n.trim().to_ascii_lowercase();
            let v = v.trim();
            let v = v.strip_suffix(" i").unwrap_or(v).trim();
            let v = v.trim_matches(|c| c == '"' || c == '\'').to_string();
            let op = match op {
                "~=" => AttrOp::Word(v),
                "^=" => AttrOp::Prefix(v),
                "$=" => AttrOp::Suffix(v),
                "*=" => AttrOp::Contains(v),
                "|=" => AttrOp::Dash(v),
                _ => AttrOp::Equals(v),
            };
            return Some((name, op));
        }
    }
    Some((inner.trim().to_ascii_lowercase(), AttrOp::Exists))
}

// ---- matching -----------------------------------------------------------------

fn element_siblings(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    match dom.nodes[node].parent {
        Some(p) => dom.nodes[p]
            .children
            .iter()
            .copied()
            .filter(|&c| matches!(dom.nodes[c].data, NodeData::Element(_)))
            .collect(),
        None => alloc::vec![node],
    }
}

fn nth_matches(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        pos == b
    } else {
        (pos - b) % a == 0 && (pos - b) / a >= 0
    }
}

fn compound_matches(dom: &Dom, node: NodeId, c: &Compound) -> bool {
    let Some(el) = dom.element(node) else {
        return false;
    };
    if let Some(t) = &c.tag {
        if *t != el.tag {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if el.attr("id") != Some(id.as_str()) {
            return false;
        }
    }
    for class in &c.classes {
        if !el.has_class(class) {
            return false;
        }
    }
    for (name, op) in &c.attrs {
        let Some(v) = el.attr(name) else {
            return false;
        };
        let ok = match op {
            AttrOp::Exists => true,
            AttrOp::Equals(x) => v == x,
            AttrOp::Word(x) => v.split_ascii_whitespace().any(|w| w == x),
            AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(x.as_str()),
            AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(x.as_str()),
            AttrOp::Contains(x) => !x.is_empty() && v.contains(x.as_str()),
            AttrOp::Dash(x) => v == x || v.starts_with(&alloc::format!("{}-", x)),
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudo {
        let ok = match p {
            Pseudo::Never => false,
            Pseudo::Always => true,
            Pseudo::Root => el.tag == "html",
            Pseudo::Link => (el.tag == "a" || el.tag == "area") && el.attr("href").is_some(),
            Pseudo::Checked => el.attr("checked").is_some() || el.attr("selected").is_some(),
            Pseudo::Empty => dom.nodes[node]
                .children
                .iter()
                .all(|&ch| match &dom.nodes[ch].data {
                    NodeData::Text(t) => t.is_empty(),
                    _ => false,
                }),
            Pseudo::Not(sel) => !matches(dom, node, sel),
            Pseudo::Is(list) => list.iter().any(|s| matches(dom, node, s)),
            _ => {
                let sibs = element_siblings(dom, node);
                let pos = sibs.iter().position(|&s| s == node).unwrap_or(0) as i32;
                let n = sibs.len() as i32;
                match p {
                    Pseudo::FirstChild => pos == 0,
                    Pseudo::LastChild => pos == n - 1,
                    Pseudo::OnlyChild => n == 1,
                    Pseudo::NthChild(a, b) => nth_matches(*a, *b, pos + 1),
                    Pseudo::NthLastChild(a, b) => nth_matches(*a, *b, n - pos),
                    Pseudo::FirstOfType => {
                        !sibs[..pos as usize].iter().any(|&s| dom.tag(s) == el.tag)
                    }
                    Pseudo::LastOfType => !sibs[pos as usize + 1..]
                        .iter()
                        .any(|&s| dom.tag(s) == el.tag),
                    _ => false,
                }
            }
        };
        if !ok {
            return false;
        }
    }
    true
}

pub fn matches(dom: &Dom, node: NodeId, sel: &Selector) -> bool {
    match_from(dom, node, &sel.parts, 0)
}

fn match_from(dom: &Dom, node: NodeId, parts: &[(Compound, Combinator)], i: usize) -> bool {
    if !compound_matches(dom, node, &parts[i].0) {
        return false;
    }
    if i + 1 == parts.len() {
        return true;
    }
    // parts[i].1 says how parts[i] relates to parts[i + 1] (to its left)
    match parts[i].1 {
        Combinator::Child => match dom.nodes[node].parent {
            Some(p) => match_from(dom, p, parts, i + 1),
            None => false,
        },
        Combinator::Descendant => {
            let mut cur = dom.nodes[node].parent;
            while let Some(p) = cur {
                if match_from(dom, p, parts, i + 1) {
                    return true;
                }
                cur = dom.nodes[p].parent;
            }
            false
        }
        Combinator::Next | Combinator::Later => {
            let sibs = element_siblings(dom, node);
            let pos = sibs.iter().position(|&s| s == node).unwrap_or(0);
            if parts[i].1 == Combinator::Next {
                pos > 0 && match_from(dom, sibs[pos - 1], parts, i + 1)
            } else {
                sibs[..pos]
                    .iter()
                    .any(|&s| match_from(dom, s, parts, i + 1))
            }
        }
    }
}

/// Elements below `root` matching a selector list (querySelectorAll).
pub fn select(dom: &Dom, root: NodeId, selectors: &str) -> Vec<NodeId> {
    let list: Vec<Selector> = split_top(selectors, ',')
        .iter()
        .filter_map(|s| parse_selector(s))
        .collect();
    dom.descendants(root)
        .into_iter()
        .filter(|&n| list.iter().any(|s| matches(dom, n, s)))
        .collect()
}
