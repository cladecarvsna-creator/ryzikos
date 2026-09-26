//! The document tree (DOM) and a forgiving HTML parser that builds it.
//!
//! Nodes live in one vector and refer to each other by index, so the tree
//! is cheap to change from JavaScript and is freed all at once.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::text::{decode_entities, find_ci, read_tag, Tag};

pub type NodeId = usize;

pub const DOCUMENT: NodeId = 0;

pub enum NodeData {
    Document,
    Element(Element),
    Text(String),
}

pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|c| c.split_ascii_whitespace().any(|c| c == class))
    }
}

pub struct Node {
    pub data: NodeData,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

pub struct Dom {
    pub nodes: Vec<Node>,
    /// Bumped on every change, so the browser knows to lay out again.
    pub version: u32,
}

/// Elements that never have content.
pub const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Elements whose content is text, not tags.
const RAW: &[&str] = &[
    "script", "style", "title", "textarea", "xmp", "noscript", "iframe", "noembed", "noframes",
];
/// Elements that belong in <head> when they come before the body.
const HEAD_ONLY: &[&str] = &[
    "title", "meta", "link", "style", "script", "base", "noscript",
];
/// Starting one of these ends an open <p>.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

impl Dom {
    pub fn new() -> Dom {
        Dom {
            nodes: alloc::vec![Node {
                data: NodeData::Document,
                parent: None,
                children: Vec::new(),
            }],
            version: 0,
        }
    }

    fn add(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node {
            data,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.add(NodeData::Element(Element {
            tag: tag.to_ascii_lowercase(),
            attrs: Vec::new(),
        }))
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.add(NodeData::Text(text.to_string()))
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes.get_mut(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self, id: NodeId) -> &str {
        self.element(id).map_or("", |e| e.tag.as_str())
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?.attr(name)
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        if let Some(e) = self.element_mut(id) {
            let name = name.to_ascii_lowercase();
            match e.attrs.iter_mut().find(|(n, _)| *n == name) {
                Some((_, v)) if v == value => return,
                Some((_, v)) => *v = value.to_string(),
                None => e.attrs.push((name, value.to_string())),
            }
            self.version += 1;
        }
    }

    /// Set a form field's value. Typing does not move anything on the
    /// page, so this does not ask for a new layout.
    pub fn set_value(&mut self, id: NodeId, value: &str) {
        if let Some(e) = self.element_mut(id) {
            match e.attrs.iter_mut().find(|(n, _)| n == "value") {
                Some((_, v)) => *v = value.to_string(),
                None => e.attrs.push((String::from("value"), value.to_string())),
            }
        }
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        if let Some(e) = self.element_mut(id) {
            e.attrs.retain(|(n, _)| n != name);
            self.version += 1;
        }
    }

    /// Take `child` out of its parent.
    pub fn detach(&mut self, child: NodeId) {
        if let Some(p) = self.nodes[child].parent.take() {
            self.nodes[p].children.retain(|&c| c != child);
            self.version += 1;
        }
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        if child == parent || self.is_ancestor(child, parent) {
            return;
        }
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        self.version += 1;
    }

    /// Insert `child` into `parent` before `before` (at the end if None).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) {
        if child == parent || self.is_ancestor(child, parent) {
            return;
        }
        self.detach(child);
        let pos = before
            .and_then(|b| self.nodes[parent].children.iter().position(|&c| c == b))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.insert(pos, child);
        self.version += 1;
    }

    fn is_ancestor(&self, a: NodeId, mut n: NodeId) -> bool {
        while let Some(p) = self.nodes[n].parent {
            if p == a {
                return true;
            }
            n = p;
        }
        false
    }

    /// A copy of a node, with its descendants when `deep`, not in the tree.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        let data = match &self.nodes[id].data {
            NodeData::Text(t) => NodeData::Text(t.clone()),
            NodeData::Element(e) => NodeData::Element(Element {
                tag: e.tag.clone(),
                attrs: e.attrs.clone(),
            }),
            NodeData::Document => NodeData::Element(Element {
                tag: String::from("#fragment"),
                attrs: Vec::new(),
            }),
        };
        let copy = self.add(data);
        if deep {
            for c in self.nodes[id].children.clone() {
                let cc = self.clone_node(c, true);
                self.nodes[cc].parent = Some(copy);
                self.nodes[copy].children.push(cc);
            }
        }
        copy
    }

    /// Whether a node is in the document.
    pub fn connected(&self, mut n: NodeId) -> bool {
        loop {
            if n == DOCUMENT {
                return true;
            }
            match self.nodes[n].parent {
                Some(p) => n = p,
                None => return false,
            }
        }
    }

    pub fn remove_children(&mut self, id: NodeId) {
        for c in core::mem::take(&mut self.nodes[id].children) {
            self.nodes[c].parent = None;
        }
        self.version += 1;
    }

    /// All text inside a node.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
        }
    }

    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        if let NodeData::Text(t) = &mut self.nodes[id].data {
            *t = text.to_string();
            self.version += 1;
            return;
        }
        self.remove_children(id);
        if !text.is_empty() {
            let t = self.create_text(text);
            self.append(id, t);
        }
    }

    /// Elements in document order below `root`.
    pub fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[root].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if matches!(self.nodes[n].data, NodeData::Element(_)) {
                out.push(n);
            }
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    pub fn find_tag(&self, root: NodeId, tag: &str) -> Option<NodeId> {
        self.descendants(root)
            .into_iter()
            .find(|&n| self.tag(n) == tag)
    }

    pub fn by_id(&self, id: &str) -> Option<NodeId> {
        self.descendants(DOCUMENT)
            .into_iter()
            .find(|&n| self.attr(n, "id") == Some(id))
    }

    pub fn html(&self) -> NodeId {
        self.find_tag(DOCUMENT, "html").unwrap_or(DOCUMENT)
    }

    pub fn body(&self) -> NodeId {
        self.find_tag(DOCUMENT, "body").unwrap_or(DOCUMENT)
    }

    pub fn head(&self) -> NodeId {
        self.find_tag(DOCUMENT, "head").unwrap_or(DOCUMENT)
    }

    pub fn title(&self) -> String {
        match self.find_tag(DOCUMENT, "title") {
            Some(t) => super::text::collapse(&self.text_content(t)),
            None => String::new(),
        }
    }

    /// Serialise the children of a node back to HTML (for innerHTML).
    pub fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for &c in &self.nodes[id].children {
            self.outer_html(c, &mut out);
        }
        out
    }

    fn outer_html(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => {
                for ch in t.chars() {
                    match ch {
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        '&' => out.push_str("&amp;"),
                        c => out.push(c),
                    }
                }
            }
            NodeData::Element(e) => {
                out.push('<');
                out.push_str(&e.tag);
                for (n, v) in &e.attrs {
                    out.push(' ');
                    out.push_str(n);
                    out.push_str("=\"");
                    out.push_str(&v.replace('"', "&quot;"));
                    out.push('"');
                }
                out.push('>');
                if !VOID.contains(&e.tag.as_str()) {
                    for &c in &self.nodes[id].children {
                        self.outer_html(c, out);
                    }
                    out.push_str("</");
                    out.push_str(&e.tag);
                    out.push('>');
                }
            }
            NodeData::Document => {}
        }
    }
}

/// Parse a whole page.
pub fn parse(source: &str) -> Dom {
    let mut dom = Dom::new();
    let html = dom.create_element("html");
    dom.append(DOCUMENT, html);
    let head = dom.create_element("head");
    dom.append(html, head);
    let body = dom.create_element("body");
    dom.append(html, body);
    let mut b = TreeBuilder {
        dom,
        stack: Vec::new(),
        html,
        head,
        body,
        in_body: false,
    };
    b.run(source);
    let mut dom = b.dom;
    dom.version = 0;
    dom
}

/// Parse HTML into the children of `parent` (innerHTML, document.write).
pub fn parse_fragment(dom: &mut Dom, parent: NodeId, source: &str) {
    let empty = Dom::new();
    let d = core::mem::replace(dom, empty);
    let mut b = TreeBuilder {
        dom: d,
        stack: alloc::vec![parent],
        html: parent,
        head: parent,
        body: parent,
        in_body: true,
    };
    b.run(source);
    *dom = b.dom;
    dom.version += 1;
}

struct TreeBuilder {
    dom: Dom,
    /// Open elements, innermost last.
    stack: Vec<NodeId>,
    html: NodeId,
    head: NodeId,
    body: NodeId,
    in_body: bool,
}

impl TreeBuilder {
    fn current(&self) -> NodeId {
        match self.stack.last() {
            Some(&n) => n,
            None if self.in_body => self.body,
            None => self.head,
        }
    }

    fn start_body(&mut self) {
        if !self.in_body {
            self.in_body = true;
            self.stack.clear();
        }
    }

    fn run(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut text_start = 0;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            // big pages parse in the background, a slice at a time
            crate::fiber::pause_if_slice_used();
            let rest = &s[i..];
            let next = bytes.get(i + 1).copied().unwrap_or(0);
            if rest.starts_with("<!--") {
                self.text(&s[text_start..i]);
                i = rest.find("-->").map_or(bytes.len(), |e| i + e + 3);
                text_start = i;
            } else if next == b'!' || next == b'?' {
                self.text(&s[text_start..i]);
                i = rest.find('>').map_or(bytes.len(), |e| i + e + 1);
                text_start = i;
            } else if next == b'/' || next.is_ascii_alphabetic() {
                self.text(&s[text_start..i]);
                let (end, tag) = read_tag(s, i);
                i = end;
                text_start = i;
                if tag.closing {
                    self.end_tag(&tag.name);
                    continue;
                }
                let name = tag.name.clone();
                let self_closing = s[..end].ends_with("/>");
                let el = self.start_tag(tag);
                if RAW.contains(&name.as_str()) {
                    let close = find_ci(&s[i..], &alloc::format!("</{}", name));
                    let content_end = close.map_or(bytes.len(), |c| i + c);
                    if let Some(el) = el {
                        let content = &s[i..content_end];
                        if !content.is_empty() {
                            let text = if matches!(name.as_str(), "title" | "textarea") {
                                decode_entities(content)
                            } else {
                                content.to_string()
                            };
                            let t = self.dom.create_text(&text);
                            self.dom.append(el, t);
                        }
                        self.stack.retain(|&n| n != el);
                    }
                    i = match close {
                        Some(_) => s[content_end..]
                            .find('>')
                            .map_or(bytes.len(), |e| content_end + e + 1),
                        None => bytes.len(),
                    };
                    text_start = i;
                } else if self_closing {
                    if let Some(el) = el {
                        // <div/> is not really self-closing in HTML, but
                        // SVG-ish markup uses it; close it to be safe
                        if name != "div" && name != "span" {
                            self.stack.retain(|&n| n != el);
                        }
                    }
                }
            } else {
                i += 1;
            }
        }
        self.text(&s[text_start..]);
    }

    fn text(&mut self, raw: &str) {
        if raw.is_empty() {
            return;
        }
        if !self.in_body {
            if raw.trim().is_empty() {
                return;
            }
            self.start_body();
        }
        let text = decode_entities(raw);
        let parent = self.current();
        // merge with a text node just before
        if let Some(&last) = self.dom.nodes[parent].children.last() {
            if let NodeData::Text(t) = &mut self.dom.nodes[last].data {
                t.push_str(&text);
                return;
            }
        }
        let t = self.dom.create_text(&text);
        self.dom.append(parent, t);
    }

    fn open_has(&self, tag: &str) -> Option<usize> {
        self.stack.iter().rposition(|&n| self.dom.tag(n) == tag)
    }

    /// Close the innermost open `tag` if it is open and nothing in
    /// `barrier` is inside it.
    fn close_if_open(&mut self, tags: &[&str], barrier: &[&str]) {
        for pos in (0..self.stack.len()).rev() {
            let t = self.dom.tag(self.stack[pos]);
            if tags.contains(&t) {
                self.stack.truncate(pos);
                return;
            }
            if barrier.contains(&t) {
                return;
            }
        }
    }

    fn start_tag(&mut self, tag: Tag) -> Option<NodeId> {
        let name = tag.name.as_str();
        match name {
            "html" => {
                self.merge_attrs(self.html, &tag);
                return None;
            }
            "head" => return None,
            "body" => {
                self.merge_attrs(self.body, &tag);
                self.start_body();
                return None;
            }
            _ => {}
        }
        if !self.in_body && !HEAD_ONLY.contains(&name) {
            self.start_body();
        }
        // implied end tags
        if CLOSES_P.contains(&name) {
            self.close_if_open(&["p"], &["button", "table", "td", "th", "li", "div"]);
        }
        match name {
            "li" => self.close_if_open(&["li"], &["ul", "ol", "menu"]),
            "dt" | "dd" => self.close_if_open(&["dt", "dd"], &["dl"]),
            "tr" => self.close_if_open(&["tr", "td", "th"], &["table", "tbody", "thead", "tfoot"]),
            "td" | "th" => self.close_if_open(&["td", "th"], &["tr", "table"]),
            "tbody" | "thead" | "tfoot" => {
                self.close_if_open(&["tbody", "thead", "tfoot"], &["table"])
            }
            "option" => self.close_if_open(&["option"], &["select"]),
            "a" => self.close_if_open(&["a"], &["div", "td", "li", "p"]),
            _ => {}
        }
        let el = self.dom.create_element(name);
        for (n, v) in &tag.attrs {
            if let Some(e) = self.dom.element_mut(el) {
                if !e.attrs.iter().any(|(x, _)| x == n) {
                    e.attrs.push((n.clone(), decode_entities(v)));
                }
            }
        }
        let parent = self.current();
        self.dom.append(parent, el);
        if !VOID.contains(&name) {
            self.stack.push(el);
            if self.stack.len() > 400 {
                self.stack.remove(0);
            }
        }
        Some(el)
    }

    fn merge_attrs(&mut self, el: NodeId, tag: &Tag) {
        for (n, v) in &tag.attrs {
            if self.dom.attr(el, n).is_none() {
                let v = decode_entities(v);
                self.dom.set_attr(el, n, &v);
            }
        }
    }

    fn end_tag(&mut self, name: &str) {
        match name {
            "html" | "body" | "head" => {
                if name == "head" {
                    self.start_body();
                }
                return;
            }
            "br" => {
                self.start_tag(Tag {
                    name: "br".into(),
                    closing: false,
                    attrs: Vec::new(),
                });
                return;
            }
            "p" if self.open_has("p").is_none() => {
                // a stray </p> makes an empty paragraph
                let p = self.start_tag(Tag {
                    name: "p".into(),
                    closing: false,
                    attrs: Vec::new(),
                });
                if let Some(p) = p {
                    self.stack.retain(|&n| n != p);
                }
                return;
            }
            _ => {}
        }
        if let Some(pos) = self.open_has(name) {
            self.stack.truncate(pos);
        }
    }
}
