//! A loaded page: its DOM, style sheets, computed styles, layout,
//! scripts and images, and what clicking and typing do to it.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use super::css::{self, Stylesheet};
use super::dom::{self, Dom, NodeData, NodeId, DOCUMENT};
use super::image::Image;
use super::layout::{self, Face, Layout, Metrics};
use super::style::{Style, Styler};
use super::url::Url;
use super::{http, text, Nav};
use crate::gui::webfont;
use crate::js::{self, Host, Value};

/// At most this many external scripts and style sheets per page.
const MAX_SCRIPTS: usize = 80;
const MAX_SHEETS: usize = 40;
const MAX_IMAGES: usize = 120;
/// Scripts may run this long while a page loads; the rest are skipped.
const SCRIPTS_MS: i64 = 8000;

/// State the scripts change besides the DOM.
#[derive(Default)]
pub struct ScriptState {
    pub nav: Option<Nav>,
    pub back: bool,
    pub scroll_to: Option<NodeId>,
    current_script: Option<NodeId>,
    /// Script elements added by scripts, to run next.
    new_scripts: Vec<NodeId>,
    executed: BTreeSet<NodeId>,
    viewport: (i32, i32),
    /// Where document.write puts its HTML while a script runs.
    write_target: Option<(NodeId, Option<NodeId>)>,
    /// A link was activated from script (element.click()).
    activated: Vec<NodeId>,
    submitted: Vec<NodeId>,
    external: usize,
}

pub struct Page {
    /// None for built-in pages.
    pub url: Option<Url>,
    pub dom: Dom,
    pub styles: Vec<Rc<Style>>,
    pub layout: Layout,
    /// DOM version the layout was made from.
    laid_out: Option<u32>,
    viewport: (i32, i32),
    js: Option<js::Context>,
    pub st: ScriptState,
    /// Downloaded style sheet text by address.
    css_text: BTreeMap<String, String>,
    /// Parsed sheets by the element they came from, with a fingerprint.
    sheets: BTreeMap<NodeId, (u64, Rc<Stylesheet>)>,
    pub images: BTreeMap<String, Image>,
    image_queue: VecDeque<String>,
    /// Images handed out for downloading.
    requested: BTreeSet<String>,
    /// Images whose size the layout is waiting for.
    size_missing: BTreeSet<String>,
    /// When the next JavaScript timer is due (ms since boot).
    next_timer: Option<i64>,
    /// Goes up whenever the layout or the images change, so the browser
    /// knows when to draw the page again.
    pub generation: u64,
    /// How long the last layout took.
    pub layout_ms: i64,
}

/// Measurements for layout: the web fonts and this page's images.
struct PageMetrics<'a> {
    images: &'a BTreeMap<String, Image>,
    /// Images the layout wanted the size of but did not have.
    missing: RefCell<BTreeSet<String>>,
}

fn face(f: Face) -> webfont::Face {
    webfont::Face {
        bold: f.bold,
        italic: f.italic,
        mono: f.mono,
    }
}

impl Metrics for PageMetrics<'_> {
    fn width16(&self, f: Face, size: f32, text: &str) -> i32 {
        webfont::width16(face(f), size, text)
    }

    fn ascent_descent(&self, f: Face, size: f32) -> (i32, i32) {
        let m = webfont::vmetrics(face(f), size);
        (m.ascent, m.descent)
    }

    fn image_size(&self, src: &str) -> Option<(i32, i32)> {
        let size = self
            .images
            .get(src)
            .map(|i| (i.width as i32, i.height as i32));
        if size.is_none() {
            self.missing.borrow_mut().insert(src.to_string());
        }
        size
    }
}

fn hash(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h ^ s.len() as u64
}

impl Page {
    pub fn new(url: Option<Url>, dom: Dom, viewport: (i32, i32)) -> Page {
        Page {
            url,
            dom,
            styles: Vec::new(),
            layout: Layout::default(),
            laid_out: None,
            viewport,
            js: None,
            st: ScriptState {
                viewport,
                ..ScriptState::default()
            },
            css_text: BTreeMap::new(),
            sheets: BTreeMap::new(),
            images: BTreeMap::new(),
            image_queue: VecDeque::new(),
            requested: BTreeSet::new(),
            size_missing: BTreeSet::new(),
            next_timer: None,
            generation: 0,
            layout_ms: 0,
        }
    }

    pub fn title(&self) -> String {
        self.dom.title()
    }

    pub fn address(&self) -> String {
        match &self.url {
            Some(u) => format!("{}", u),
            None => String::from(super::HOME),
        }
    }

    /// The base address links are resolved against.
    fn base(&self) -> Option<Url> {
        let u = self.url.as_ref()?;
        let href = self
            .dom
            .find_tag(self.dom.head(), "base")
            .and_then(|b| self.dom.attr(b, "href"));
        Some(match href {
            Some(h) => u.join(h).unwrap_or_else(|| u.clone()),
            None => u.clone(),
        })
    }

    /// Resolve a link on this page.
    pub fn resolve(&self, link: &str) -> Option<Url> {
        match self.base() {
            Some(b) => b.join(link),
            None => Url::parse(link),
        }
    }

    // ---- styles and layout ----------------------------------------------------------

    /// The page's style sheets in document order, downloading new ones.
    fn collect_sheets(&mut self) -> Vec<Rc<Stylesheet>> {
        let media = css::Media {
            width: self.viewport.0,
            height: self.viewport.1,
        };
        let nodes: Vec<NodeId> = self
            .dom
            .descendants(DOCUMENT)
            .into_iter()
            .filter(|&n| match self.dom.tag(n) {
                "style" => true,
                "link" => self.dom.attr(n, "rel").is_some_and(|r| {
                    r.split_ascii_whitespace()
                        .any(|w| w.eq_ignore_ascii_case("stylesheet"))
                        && !r.to_ascii_lowercase().contains("alternate")
                }),
                _ => false,
            })
            .collect();
        let mut out = Vec::new();
        for n in nodes {
            if self.dom.attr(n, "media").is_some_and(|m| {
                let m = m.to_ascii_lowercase();
                m.contains("print") && !m.contains("screen") && !m.contains("all")
            }) {
                continue;
            }
            let source = if self.dom.tag(n) == "style" {
                self.dom.text_content(n)
            } else {
                let Some(href) = self.dom.attr(n, "href").map(|h| h.to_string()) else {
                    continue;
                };
                let Some(u) = self.resolve(&href) else {
                    continue;
                };
                let key = u.to_string();
                if !self.css_text.contains_key(&key) {
                    if self.css_text.len() >= MAX_SHEETS {
                        continue;
                    }
                    let text = fetch_text(&u).unwrap_or_default();
                    let text = self.inline_imports(&u, text);
                    self.css_text.insert(key.clone(), text);
                }
                self.css_text[&key].clone()
            };
            let h = hash(&source);
            match self.sheets.get(&n) {
                Some((old, sheet)) if *old == h => out.push(sheet.clone()),
                _ => {
                    let sheet = Rc::new(css::parse_stylesheet(&source, &media));
                    self.sheets.insert(n, (h, sheet.clone()));
                    out.push(sheet);
                }
            }
        }
        out
    }

    /// Put the text of @import-ed sheets in front of a sheet (one level).
    fn inline_imports(&mut self, base: &Url, text: String) -> String {
        let media = css::Media {
            width: self.viewport.0,
            height: self.viewport.1,
        };
        let imports = css::parse_stylesheet(&text, &media).imports;
        if imports.is_empty() {
            return text;
        }
        let mut out = String::new();
        for i in imports.iter().take(8) {
            if let Some(u) = base.join(i) {
                if let Some(t) = fetch_text(&u) {
                    out.push_str(&t);
                    out.push('\n');
                }
            }
        }
        out.push_str(&text);
        out
    }

    /// Restyle and lay out again if the DOM changed.
    pub fn update(&mut self) -> bool {
        if self.laid_out == Some(self.dom.version) {
            return false;
        }
        let t0 = js::now_ms();
        let sheets = self.collect_sheets();
        let t1 = js::now_ms();
        let styler = Styler::new(sheets, self.viewport);
        self.styles = styler.compute(&self.dom);
        let t2 = js::now_ms();
        let m = PageMetrics {
            images: &self.images,
            missing: RefCell::new(BTreeSet::new()),
        };
        self.layout = layout::layout(&self.dom, &self.styles, self.viewport, &m);
        self.size_missing = m.missing.into_inner();
        let t3 = js::now_ms();
        self.layout_ms = t3 - t0;
        if t3 - t0 > 50 {
            log(&format!(
                "layout of {} nodes took {} ms (sheets {}, styles {}, boxes {})",
                self.dom.nodes.len(),
                t3 - t0,
                t1 - t0,
                t2 - t1,
                t3 - t2
            ));
        }
        self.laid_out = Some(self.dom.version);
        self.generation += 1;
        self.queue_images();
        true
    }

    // ---- images ---------------------------------------------------------------------

    fn queue_images(&mut self) {
        let mut wanted = Vec::new();
        for it in &self.layout.items {
            if let layout::Item::Image { src, .. } = it {
                if !self.images.contains_key(src)
                    && !self.requested.contains(src)
                    && !self.image_queue.contains(src)
                    && !wanted.contains(src)
                {
                    wanted.push(src.clone());
                }
            }
        }
        for w in wanted {
            if self.requested.len() + self.image_queue.len() < MAX_IMAGES {
                self.image_queue.push_back(w);
            }
        }
    }

    /// The images waiting to be downloaded, with their addresses (None
    /// for data: URLs). They come back through [`Page::add_image`].
    pub fn take_image_queue(&mut self) -> Vec<(String, Option<Url>)> {
        let queue = core::mem::take(&mut self.image_queue);
        queue
            .into_iter()
            .map(|src| {
                let u = if src.starts_with("data:") {
                    None
                } else {
                    self.resolve(&src)
                };
                self.requested.insert(src.clone());
                (src, u)
            })
            .collect()
    }

    /// An image arrived. Returns true if the page must be laid out again
    /// because the layout was waiting for the image's size.
    pub fn add_image(&mut self, src: String, img: Image) -> bool {
        let shown = img.width > 0;
        let relayout = shown && self.size_missing.remove(&src);
        self.images.insert(src, img);
        if shown {
            // drawn from the next frame on
            self.generation += 1;
        }
        if relayout {
            self.laid_out = None;
        }
        relayout
    }

    // ---- scripts ------------------------------------------------------------------------

    fn run_js<R>(&mut self, f: impl FnOnce(&mut js::Context, &mut PageHost) -> R) -> Option<R> {
        let ctx = self.js.as_mut()?;
        let base = self.url.as_ref().map(|u| {
            let href = self
                .dom
                .find_tag(self.dom.head(), "base")
                .and_then(|b| self.dom.attr(b, "href"));
            match href {
                Some(h) => u.join(h).unwrap_or_else(|| u.clone()),
                None => u.clone(),
            }
        });
        let mut host = PageHost {
            dom: &mut self.dom,
            url: &mut self.url,
            base,
            layout: &self.layout,
            styles: &self.styles,
            st: &mut self.st,
        };
        Some(f(ctx, &mut host))
    }

    /// Start JavaScript and run the page's scripts in order.
    pub fn run_scripts(&mut self) {
        let Some(mut ctx) = js::Context::new() else {
            log("could not start JavaScript: out of memory");
            return;
        };
        {
            let mut host = crate::js::ConsoleHost;
            let _ = ctx.eval(&mut host, js::BASE_PRELUDE, "<prelude>");
        }
        self.js = Some(ctx);
        if let Some(Err(e)) = self.run_js(|c, h| c.eval(h, RUNTIME, "<runtime>")) {
            log(&format!("runtime failed: {}", e));
            self.js = None;
            return;
        }
        let scripts: Vec<NodeId> = self
            .dom
            .descendants(DOCUMENT)
            .into_iter()
            .filter(|&n| self.dom.tag(n) == "script")
            .collect();
        let started = js::now_ms();
        for s in scripts {
            if js::now_ms() - started > SCRIPTS_MS {
                log("scripts took too long; showing the page without the rest");
                break;
            }
            self.execute(s);
            self.run_new_scripts();
        }
        self.eval_quiet("__everos_loaded('interactive')", "<load>");
        self.eval_quiet("__everos_loaded('complete')", "<load>");
        self.run_timers(true);
    }

    fn run_new_scripts(&mut self) {
        for _ in 0..50 {
            let new = core::mem::take(&mut self.st.new_scripts);
            if new.is_empty() {
                break;
            }
            for s in new {
                self.execute(s);
            }
        }
    }

    /// Run one script element, once.
    fn execute(&mut self, n: NodeId) {
        if !self.st.executed.insert(n) || !self.dom.connected(n) {
            return;
        }
        let ty = self
            .dom
            .attr(n, "type")
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let module = ty == "module";
        let classic = matches!(
            ty.as_str(),
            "" | "text/javascript"
                | "application/javascript"
                | "application/ecmascript"
                | "text/ecmascript"
                | "text/babel-disabled"
        ) || ty.starts_with("text/javascript");
        if !(classic || module) {
            return;
        }
        if classic && self.dom.attr(n, "nomodule").is_some() {
            return;
        }
        let (source, name) = match self.dom.attr(n, "src").map(|s| s.to_string()) {
            Some(src) => {
                if self.st.external >= MAX_SCRIPTS {
                    return;
                }
                self.st.external += 1;
                let Some(u) = self.resolve(&src) else {
                    return;
                };
                match fetch_text(&u) {
                    Some(t) => (t, u.to_string()),
                    None => {
                        log(&format!("could not load script {}", u));
                        return;
                    }
                }
            }
            None => (
                self.dom.text_content(n),
                format!("{}#inline", self.address()),
            ),
        };
        self.st.current_script = Some(n);
        let parent = self.dom.nodes[n].parent.unwrap_or(DOCUMENT);
        let next = self.dom.nodes[parent]
            .children
            .iter()
            .position(|&c| c == n)
            .and_then(|i| self.dom.nodes[parent].children.get(i + 1).copied());
        self.st.write_target = Some((parent, next));
        let set_current = format!("globalThis.__currentScript = {};", n);
        self.eval_quiet(&set_current, "<script>");
        let t0 = js::now_ms();
        let r = self.run_js(|c, h| {
            if module {
                c.eval_module(h, &source, &name)
            } else {
                c.eval(h, &source, &name)
            }
        });
        if let Some(Err(e)) = r {
            log(&format!("error in {}: {}", name, e));
        }
        log(&format!(
            "ran {} ({} KB) in {} ms",
            name,
            source.len() / 1024,
            js::now_ms() - t0
        ));
        self.eval_quiet("globalThis.__currentScript = null;", "<script>");
        self.st.current_script = None;
        self.st.write_target = None;
    }

    fn eval_quiet(&mut self, code: &str, name: &str) -> Option<String> {
        match self.run_js(|c, h| c.eval(h, code, name))? {
            Ok(v) => Some(v),
            Err(e) => {
                log(&format!("error: {}", e));
                None
            }
        }
    }

    /// Run due timers. Returns true if anything ran.
    pub fn run_timers(&mut self, force: bool) -> bool {
        if self.js.is_none() {
            return false;
        }
        let now = js::now_ms();
        if !force && self.next_timer.is_none_or(|t| t > now) {
            return false;
        }
        let next = self
            .eval_quiet("__everos_timers()", "<timers>")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(-1);
        self.next_timer = if next >= 0 {
            Some(js::now_ms() + next)
        } else {
            None
        };
        self.run_new_scripts();
        self.handle_script_actions();
        true
    }

    /// Clicks and submits that scripts started.
    fn handle_script_actions(&mut self) {
        for n in core::mem::take(&mut self.st.activated) {
            if self.st.nav.is_none() {
                if let Some(nav) = self.default_action(n) {
                    self.st.nav = Some(nav);
                }
            }
        }
        for f in core::mem::take(&mut self.st.submitted) {
            if self.st.nav.is_none() {
                self.st.nav = self.form_nav(f, None);
            }
        }
    }

    /// Fire an event at a node. Returns false when a handler cancelled it.
    pub fn dispatch(&mut self, node: NodeId, event: &str, x: i32, y: i32) -> bool {
        if self.js.is_none() {
            return true;
        }
        let r = self.eval_quiet(
            &format!("__everos_dispatch({}, '{}', {}, {})", node, event, x, y),
            "<event>",
        );
        self.run_new_scripts();
        self.handle_script_actions();
        // setTimeout(..., 0) in a handler should run right away
        self.next_timer = Some(0);
        r.is_none_or(|v| v.trim() != "0")
    }

    pub fn key_event(&mut self, node: Option<NodeId>, key: &str) -> bool {
        if self.js.is_none() {
            return true;
        }
        let key = key.replace('\\', "\\\\").replace('\'', "\\'");
        let target = node.map_or(-1, |n| n as i64);
        let r = self.eval_quiet(
            &format!("__everos_key({}, '{}', true)", target, key),
            "<key>",
        );
        self.next_timer = Some(0);
        r.is_none_or(|v| v.trim() != "0")
    }

    // ---- clicking and forms ---------------------------------------------------------

    /// The element at a point, for events: text runs report their parent.
    pub fn element_at(&self, x: i32, y: i32) -> Option<NodeId> {
        let n = self.layout.hit(x, y)?;
        Some(match self.dom.nodes[n].data {
            NodeData::Text(_) => self.dom.nodes[n].parent.unwrap_or(n),
            _ => n,
        })
    }

    /// The link (a element) a node is in.
    pub fn link_of(&self, mut n: NodeId) -> Option<NodeId> {
        loop {
            if self.dom.tag(n) == "a" && self.dom.attr(n, "href").is_some() {
                return Some(n);
            }
            n = self.dom.nodes[n].parent?;
        }
    }

    /// Whether clicking `node` follows a link that opens in a new tab.
    pub fn opens_new_tab(&self, node: NodeId) -> bool {
        self.link_of(node)
            .and_then(|a| self.dom.attr(a, "target"))
            .is_some_and(|t| t.eq_ignore_ascii_case("_blank"))
    }

    pub fn link_target(&self, a: NodeId) -> Option<String> {
        let href = self.dom.attr(a, "href")?;
        Some(match self.resolve(href) {
            Some(u) => u.to_string(),
            None => href.to_string(),
        })
    }

    fn ancestor_tag(&self, mut n: NodeId, tag: &str) -> Option<NodeId> {
        loop {
            if self.dom.tag(n) == tag {
                return Some(n);
            }
            n = self.dom.nodes[n].parent?;
        }
    }

    /// What clicking a node does when no script stops it.
    pub fn default_action(&mut self, node: NodeId) -> Option<Nav> {
        let mut n = node;
        loop {
            let tag = self.dom.tag(n).to_string();
            match tag.as_str() {
                "a" if self.dom.attr(n, "href").is_some() => {
                    let href = self.dom.attr(n, "href").unwrap().trim().to_string();
                    if let Some(code) = href.strip_prefix("javascript:") {
                        let code = super::url::decode_percent(code);
                        self.eval_quiet(&code, "<link>");
                        self.handle_script_actions();
                        return self.st.nav.take();
                    }
                    if let Some(frag) = href.strip_prefix('#') {
                        self.st.scroll_to = if frag.is_empty() {
                            Some(DOCUMENT)
                        } else {
                            self.dom.by_id(frag).or_else(|| {
                                self.dom.descendants(DOCUMENT).into_iter().find(|&e| {
                                    self.dom.tag(e) == "a" && self.dom.attr(e, "name") == Some(frag)
                                })
                            })
                        };
                        return None;
                    }
                    if href.eq_ignore_ascii_case(super::HOME) {
                        return Some(Nav::Home);
                    }
                    return self.resolve(&href).map(Nav::Get);
                }
                "input" | "button" => {
                    let ty = self
                        .dom
                        .attr(n, "type")
                        .unwrap_or(if tag == "button" { "submit" } else { "text" })
                        .to_ascii_lowercase();
                    match ty.as_str() {
                        "submit" | "image" => {
                            let form = self.form_of(n)?;
                            return self.submit(form, Some(n));
                        }
                        "checkbox" => {
                            if self.dom.attr(n, "checked").is_some() {
                                self.dom.remove_attr(n, "checked");
                            } else {
                                self.dom.set_attr(n, "checked", "");
                            }
                            self.dispatch(n, "input", 0, 0);
                            self.dispatch(n, "change", 0, 0);
                            return None;
                        }
                        "radio" => {
                            if let Some(name) = self.dom.attr(n, "name").map(|s| s.to_string()) {
                                for r in self.dom.descendants(DOCUMENT) {
                                    if self.dom.tag(r) == "input"
                                        && self.dom.attr(r, "name") == Some(name.as_str())
                                    {
                                        self.dom.remove_attr(r, "checked");
                                    }
                                }
                            }
                            self.dom.set_attr(n, "checked", "");
                            self.dispatch(n, "change", 0, 0);
                            return None;
                        }
                        "reset" | "button" => return None,
                        _ => return None,
                    }
                }
                "select" => {
                    // step to the next option
                    let opts: Vec<NodeId> = self
                        .dom
                        .descendants(n)
                        .into_iter()
                        .filter(|&o| self.dom.tag(o) == "option")
                        .collect();
                    if !opts.is_empty() {
                        let cur = opts
                            .iter()
                            .position(|&o| self.dom.attr(o, "selected").is_some())
                            .unwrap_or(0);
                        for &o in &opts {
                            self.dom.remove_attr(o, "selected");
                        }
                        self.dom
                            .set_attr(opts[(cur + 1) % opts.len()], "selected", "");
                        self.dispatch(n, "change", 0, 0);
                    }
                    return None;
                }
                "summary" => {
                    if let Some(d) = self.dom.nodes[n]
                        .parent
                        .filter(|&p| self.dom.tag(p) == "details")
                    {
                        if self.dom.attr(d, "open").is_some() {
                            self.dom.remove_attr(d, "open");
                        } else {
                            self.dom.set_attr(d, "open", "");
                        }
                    }
                    return None;
                }
                "label" => {
                    let target = match self.dom.attr(n, "for") {
                        Some(id) => self.dom.by_id(id),
                        None => self
                            .dom
                            .descendants(n)
                            .into_iter()
                            .find(|&c| self.dom.tag(c) == "input"),
                    };
                    if let Some(t) = target {
                        if t != node {
                            return self.default_action(t);
                        }
                    }
                    return None;
                }
                _ => {}
            }
            n = self.dom.nodes[n].parent?;
        }
    }

    pub fn form_of(&self, n: NodeId) -> Option<NodeId> {
        if let Some(id) = self.dom.attr(n, "form") {
            return self.dom.by_id(id);
        }
        self.ancestor_tag(n, "form")
    }

    /// Submit a form: fire its submit event, then build the request.
    pub fn submit(&mut self, form: NodeId, submitter: Option<NodeId>) -> Option<Nav> {
        if !self.dispatch(form, "submit", 0, 0) {
            return self.st.nav.take();
        }
        self.form_nav(form, submitter)
    }

    /// The value of a form field.
    pub fn field_value(&self, n: NodeId) -> String {
        match self.dom.tag(n) {
            "textarea" => self
                .dom
                .attr(n, "value")
                .map(|v| v.to_string())
                .unwrap_or_else(|| self.dom.text_content(n)),
            "select" => {
                let opts: Vec<NodeId> = self
                    .dom
                    .descendants(n)
                    .into_iter()
                    .filter(|&o| self.dom.tag(o) == "option")
                    .collect();
                let sel = opts
                    .iter()
                    .copied()
                    .find(|&o| self.dom.attr(o, "selected").is_some())
                    .or_else(|| opts.first().copied());
                match sel {
                    Some(o) => self
                        .dom
                        .attr(o, "value")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| text::collapse(&self.dom.text_content(o))),
                    None => String::new(),
                }
            }
            _ => self.dom.attr(n, "value").unwrap_or("").to_string(),
        }
    }

    fn form_nav(&mut self, form: NodeId, submitter: Option<NodeId>) -> Option<Nav> {
        let mut query = String::new();
        let mut add = |k: &str, v: &str| {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str(&super::url::encode_query(k));
            query.push('=');
            query.push_str(&super::url::encode_query(v));
        };
        let fields: Vec<NodeId> = self
            .dom
            .descendants(DOCUMENT)
            .into_iter()
            .filter(|&n| matches!(self.dom.tag(n), "input" | "select" | "textarea" | "button"))
            .filter(|&n| self.form_of(n) == Some(form))
            .collect();
        for f in fields {
            let Some(name) = self.dom.attr(f, "name").map(|s| s.to_string()) else {
                continue;
            };
            if name.is_empty() || self.dom.attr(f, "disabled").is_some() {
                continue;
            }
            let tag = self.dom.tag(f);
            let ty = self
                .dom
                .attr(f, "type")
                .unwrap_or(if tag == "button" { "submit" } else { "text" })
                .to_ascii_lowercase();
            match ty.as_str() {
                "checkbox" | "radio" => {
                    if self.dom.attr(f, "checked").is_some() {
                        add(&name, self.dom.attr(f, "value").unwrap_or("on"));
                    }
                }
                "submit" | "image" | "button" | "reset" => {
                    if Some(f) == submitter && ty == "submit" {
                        add(&name, self.dom.attr(f, "value").unwrap_or(""));
                    }
                }
                "file" => {}
                _ => {
                    let v = self.field_value(f);
                    add(&name, &v);
                }
            }
        }
        let action = submitter
            .and_then(|s| self.dom.attr(s, "formaction"))
            .or_else(|| self.dom.attr(form, "action"))
            .map(|a| a.to_string())
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| self.address());
        let post = self
            .dom
            .attr(form, "method")
            .is_some_and(|m| m.eq_ignore_ascii_case("post"));
        let mut target = self.resolve(&action)?;
        if post {
            Some(Nav::Post(target, query))
        } else {
            let base = target.path.split('?').next().unwrap_or("/").to_string();
            target.path = format!("{}?{}", base, query);
            Some(Nav::Get(target))
        }
    }

    /// Set a field's text (typing in the browser) and tell the scripts.
    pub fn set_field(&mut self, n: NodeId, value: &str) {
        if self.dom.tag(n) == "input" {
            self.dom.set_value(n, value);
            self.generation += 1;
        } else {
            self.dom.set_attr(n, "value", value);
        }
        self.dispatch(n, "input", 0, 0);
    }

    /// Where an element is on the page, for scrolling to it.
    pub fn element_top(&self, n: NodeId) -> Option<i32> {
        if n == DOCUMENT {
            return Some(0);
        }
        self.layout.boxes.get(&n).map(|r| r.y)
    }
}

fn log(msg: &str) {
    crate::serial::write_str("\njs: ");
    crate::serial::write_str(msg);
    crate::serial::write_str("\n");
}

/// Download a text resource (script or style sheet).
pub fn fetch_text(u: &Url) -> Option<String> {
    match http::get(u, None) {
        Ok(r) if (200..300).contains(&r.status) => Some(text::decode(&r.body, &r.content_type)),
        Ok(r) => {
            log(&format!("{} answered {}", u, r.status));
            None
        }
        Err(e) => {
            log(&format!("{}: {}", u, e));
            None
        }
    }
}

/// Download and decode an image; failures give an empty image, so they
/// are not tried again.
pub fn fetch_image(src: &str, u: Option<&Url>) -> Image {
    let img = match (src.strip_prefix("data:"), u) {
        (Some(data), _) => super::image::decode_data_url(data),
        (None, Some(u)) => http::get(u, None)
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| super::image::decode(&r.body)),
        _ => None,
    };
    img.unwrap_or_else(Image::empty)
}

const RUNTIME: &str = include_str!("runtime.js");

// ---- the scripts' view of the page ------------------------------------------------------

/// Local storage, kept while EverOS runs: (origin, key) to value.
static STORAGE: crate::sync::IrqMutex<BTreeMap<(String, String), String>> =
    crate::sync::IrqMutex::new(BTreeMap::new());

struct PageHost<'a> {
    dom: &'a mut Dom,
    url: &'a mut Option<Url>,
    base: Option<Url>,
    layout: &'a Layout,
    styles: &'a [Rc<Style>],
    st: &'a mut ScriptState,
}

fn arg<'a>(args: &[Option<&'a str>], i: usize) -> &'a str {
    args.get(i).copied().flatten().unwrap_or("")
}

fn id_arg(args: &[Option<&str>], i: usize) -> Option<NodeId> {
    arg(args, i)
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|&v| v >= 0)
        .map(|v| v as usize)
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn ids(v: impl IntoIterator<Item = NodeId>) -> Value {
    let mut s = String::new();
    for (i, n) in v.into_iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{}", n));
    }
    Value::Str(s)
}

impl PageHost<'_> {
    fn valid(&self, n: NodeId) -> bool {
        n < self.dom.nodes.len()
    }

    fn resolve(&self, link: &str) -> Option<Url> {
        match &self.base {
            Some(b) => b.join(link),
            None => Url::parse(link),
        }
    }

    fn origin(&self) -> String {
        self.url
            .as_ref()
            .map_or(String::from("about:home"), |u| u.origin())
    }

    /// Note script elements that were just put into the document.
    fn inserted(&mut self, n: NodeId) {
        if !self.dom.connected(n) {
            return;
        }
        let mut stack = alloc::vec![n];
        while let Some(x) = stack.pop() {
            if self.dom.tag(x) == "script" && !self.st.executed.contains(&x) {
                self.st.new_scripts.push(x);
            }
            stack.extend(self.dom.nodes[x].children.iter().copied());
        }
    }

    fn fragment_children(&self, n: NodeId) -> Option<Vec<NodeId>> {
        (self.dom.tag(n) == "#fragment").then(|| self.dom.nodes[n].children.clone())
    }
}

impl Host for PageHost<'_> {
    fn call(&mut self, op: &str, a: &[Option<&str>]) -> Value {
        let node = id_arg(a, 0).filter(|&n| self.valid(n));
        match op {
            "log" => {
                log(arg(a, 0));
                Value::Undefined
            }
            "now" => Value::Int(js::now_ms()),
            "type" => match node.map(|n| &self.dom.nodes[n].data) {
                Some(NodeData::Element(_)) => Value::Int(1),
                Some(NodeData::Text(_)) => Value::Int(3),
                Some(NodeData::Document) => Value::Int(9),
                None => Value::Int(0),
            },
            "tag" => Value::Str(node.map_or(String::new(), |n| self.dom.tag(n).to_string())),
            "parent" => Value::Int(
                node.and_then(|n| self.dom.nodes[n].parent)
                    .map_or(-1, |p| p as i64),
            ),
            "kids" => ids(node.map_or(Vec::new(), |n| self.dom.nodes[n].children.clone())),
            "sibling" => {
                let Some(n) = node else {
                    return Value::Int(-1);
                };
                let Some(p) = self.dom.nodes[n].parent else {
                    return Value::Int(-1);
                };
                let kids = &self.dom.nodes[p].children;
                let i = kids.iter().position(|&c| c == n).unwrap_or(0) as i64;
                let j = i + if arg(a, 1).starts_with('-') { -1 } else { 1 };
                Value::Int(if j >= 0 && (j as usize) < kids.len() {
                    kids[j as usize] as i64
                } else {
                    -1
                })
            }
            "connected" => Value::Bool(node.is_some_and(|n| self.dom.connected(n))),
            "text" => Value::Str(node.map_or(String::new(), |n| self.dom.text_content(n))),
            "settext" => {
                if let Some(n) = node {
                    self.dom.set_text_content(n, arg(a, 1));
                }
                Value::Undefined
            }
            "attr" => match node.and_then(|n| self.dom.attr(n, arg(a, 1))) {
                Some(v) => Value::Str(v.to_string()),
                None => Value::Null,
            },
            "setattr" => {
                if let Some(n) = node {
                    self.dom.set_attr(n, arg(a, 1), arg(a, 2));
                }
                Value::Undefined
            }
            "rmattr" => {
                if let Some(n) = node {
                    self.dom.remove_attr(n, arg(a, 1));
                }
                Value::Undefined
            }
            "attrs" => {
                let mut s = String::from("[");
                if let Some(e) = node.and_then(|n| self.dom.element(n)) {
                    for (i, (k, v)) in e.attrs.iter().enumerate() {
                        if i > 0 {
                            s.push(',');
                        }
                        s.push('[');
                        s.push_str(&json_str(k));
                        s.push(',');
                        s.push_str(&json_str(v));
                        s.push(']');
                    }
                }
                s.push(']');
                Value::Json(s)
            }
            "create" => Value::Int(self.dom.create_element(arg(a, 0)) as i64),
            "ctext" => Value::Int(self.dom.create_text(arg(a, 0)) as i64),
            "insert" => {
                let (Some(p), Some(c)) = (node, id_arg(a, 1).filter(|&c| self.valid(c))) else {
                    return Value::Error(String::from("bad node"));
                };
                let before =
                    id_arg(a, 2).filter(|&r| self.valid(r) && self.dom.nodes[r].parent == Some(p));
                let kids = self.fragment_children(c).unwrap_or_else(|| alloc::vec![c]);
                for k in kids {
                    self.dom.insert_before(p, k, before);
                    self.inserted(k);
                }
                Value::Undefined
            }
            "remove" => {
                if let Some(n) = node {
                    self.dom.detach(n);
                }
                Value::Undefined
            }
            "inner" => Value::Str(node.map_or(String::new(), |n| self.dom.inner_html(n))),
            "outer" => Value::Str(node.map_or(String::new(), |n| {
                let wrapper = self.dom.inner_html(n);
                let _ = wrapper;
                let mut out = String::new();
                let tag = self.dom.tag(n).to_string();
                if tag.is_empty() {
                    return self.dom.text_content(n);
                }
                out.push('<');
                out.push_str(&tag);
                if let Some(e) = self.dom.element(n) {
                    for (k, v) in &e.attrs {
                        out.push_str(&format!(" {}=\"{}\"", k, v.replace('"', "&quot;")));
                    }
                }
                out.push('>');
                if !dom::VOID.contains(&tag.as_str()) {
                    out.push_str(&self.dom.inner_html(n));
                    out.push_str(&format!("</{}>", tag));
                }
                out
            })),
            "setinner" => {
                if let Some(n) = node {
                    self.dom.remove_children(n);
                    dom::parse_fragment(self.dom, n, arg(a, 1));
                    // scripts added by innerHTML do not run, as in browsers
                    let added = self.dom.descendants(n);
                    for s in added {
                        if self.dom.tag(s) == "script" {
                            self.st.executed.insert(s);
                        }
                    }
                }
                Value::Undefined
            }
            "clone" => match node {
                Some(n) => Value::Int(self.dom.clone_node(n, arg(a, 1) == "1") as i64),
                None => Value::Int(-1),
            },
            "byid" => Value::Int(self.dom.by_id(arg(a, 0)).map_or(-1, |n| n as i64)),
            "qs" | "qsa" => {
                let Some(root) = node else {
                    return Value::Null;
                };
                let sel = arg(a, 1);
                if !sel.trim().is_empty()
                    && css::split_top(sel, ',')
                        .iter()
                        .any(|s| css::parse_selector(s).is_none())
                {
                    // unsupported selectors match nothing rather than throwing
                    log(&format!("selector not supported: {}", sel));
                }
                let found = css::select(self.dom, root, sel);
                if op == "qs" {
                    Value::Int(found.first().map_or(-1, |&n| n as i64))
                } else {
                    ids(found)
                }
            }
            "matches" => {
                let Some(n) = node else {
                    return Value::Bool(false);
                };
                let sel = arg(a, 1);
                Value::Bool(
                    css::split_top(sel, ',')
                        .iter()
                        .filter_map(|s| css::parse_selector(s))
                        .any(|s| css::matches(self.dom, n, &s)),
                )
            }
            "root" => {
                let n = match arg(a, 0) {
                    "head" => self.dom.head(),
                    "body" => self.dom.body(),
                    _ => self.dom.html(),
                };
                Value::Int(n as i64)
            }
            "title" => Value::Str(self.dom.title()),
            "settitle" => {
                let head = self.dom.head();
                let t = match self.dom.find_tag(head, "title") {
                    Some(t) => t,
                    None => {
                        let t = self.dom.create_element("title");
                        self.dom.append(head, t);
                        t
                    }
                };
                self.dom.set_text_content(t, arg(a, 0));
                Value::Undefined
            }
            "write" => {
                if let Some((parent, before)) = self.st.write_target {
                    let holder = self.dom.create_element("div");
                    dom::parse_fragment(self.dom, holder, arg(a, 0));
                    for k in self.dom.nodes[holder].children.clone() {
                        self.dom.insert_before(parent, k, before);
                        self.inserted(k);
                    }
                }
                Value::Undefined
            }
            "navigate" => {
                let target = arg(a, 0);
                match self.resolve(target) {
                    Some(u) => self.st.nav = Some(Nav::Get(u)),
                    None => log(&format!("cannot navigate to {}", target)),
                }
                Value::Undefined
            }
            "back" => {
                self.st.back = true;
                Value::Undefined
            }
            "seturl" => {
                if let Some(u) = Url::parse(arg(a, 0)) {
                    *self.url = Some(u);
                }
                Value::Undefined
            }
            "sethash" => Value::Undefined,
            "location" => Value::Str(match self.url.as_ref() {
                Some(u) => u.to_string(),
                None => String::from("about:home"),
            }),
            "resolve" => {
                let link = arg(a, 0);
                Value::Str(match self.resolve(link) {
                    Some(u) => u.to_string(),
                    None => link.to_string(),
                })
            }
            "urljoin" => {
                let (base, link) = (a.first().copied().flatten(), arg(a, 1));
                let lower = link.trim().to_ascii_lowercase();
                if lower.contains(':')
                    && !lower.starts_with("http")
                    && lower.split(':').next().is_some_and(|s| {
                        s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-')
                    })
                {
                    // other schemes: blob:, data:, mailto: and so on
                    return Value::Str(link.trim().to_string());
                }
                let joined = match base {
                    Some(b) => Url::parse_absolute(b).and_then(|b| b.join(link)),
                    None => Url::parse_absolute(link),
                };
                match joined {
                    Some(u) => {
                        let hash = link.find('#').map(|i| &link[i..]).unwrap_or("");
                        Value::Str(format!("{}{}", u, hash))
                    }
                    None => Value::Null,
                }
            }
            "http" => {
                let method = arg(a, 0).to_ascii_uppercase();
                let Some(u) = self.resolve(arg(a, 1)) else {
                    return Value::Json(String::from("{\"error\":\"bad address\"}"));
                };
                let body = a.get(2).copied().flatten();
                let ty = a
                    .get(3)
                    .copied()
                    .flatten()
                    .unwrap_or("text/plain;charset=UTF-8");
                let method = if method.is_empty() {
                    String::from("GET")
                } else {
                    method
                };
                log(&format!("fetch {} {}", method, u));
                match http::request(&method, &u, body.map(|b| (ty, b.as_bytes()))) {
                    Ok(r) => {
                        let text = text::decode(&r.body, &r.content_type);
                        Value::Json(format!(
                            "{{\"status\":{},\"url\":{},\"type\":{},\"body\":{}}}",
                            r.status,
                            json_str(&r.url.to_string()),
                            json_str(&r.content_type),
                            json_str(&text)
                        ))
                    }
                    Err(e) => Value::Json(format!("{{\"error\":{}}}", json_str(&e))),
                }
            }
            "cookie" => Value::Str(
                self.url
                    .as_ref()
                    .map_or(String::new(), |u| http::cookies(&u.host)),
            ),
            "setcookie" => {
                if let Some(u) = self.url.as_ref() {
                    http::store_cookie(&u.host, arg(a, 0));
                }
                Value::Undefined
            }
            "storage" => {
                let origin = format!("{}|{}", arg(a, 0), self.origin());
                let key = arg(a, 2).to_string();
                let mut store = STORAGE.lock();
                match arg(a, 1) {
                    "get" => match store.get(&(origin, key)) {
                        Some(v) => Value::Str(v.clone()),
                        None => Value::Null,
                    },
                    "set" => {
                        store.insert((origin, key), arg(a, 3).to_string());
                        Value::Undefined
                    }
                    "remove" => {
                        store.remove(&(origin, key));
                        Value::Undefined
                    }
                    "clear" => {
                        store.retain(|(o, _), _| *o != origin);
                        Value::Undefined
                    }
                    _ => {
                        let keys: Vec<String> = store
                            .keys()
                            .filter(|(o, _)| *o == origin)
                            .map(|(_, k)| json_str(k))
                            .collect();
                        Value::Json(format!("[{}]", keys.join(",")))
                    }
                }
            }
            "media" => Value::Bool(css::media_query(
                arg(a, 0),
                &css::Media {
                    width: self.st.viewport.0,
                    height: self.st.viewport.1,
                },
            )),
            "viewport" => Value::Json(format!("[{},{}]", self.st.viewport.0, self.st.viewport.1)),
            "rect" => match node.and_then(|n| self.layout.boxes.get(&n)) {
                Some(r) => Value::Json(format!("[{},{},{},{}]", r.x, r.y, r.w, r.h)),
                None => Value::Null,
            },
            "hit" => {
                let (x, y) = (
                    arg(a, 0).parse::<f32>().unwrap_or(0.0) as i32,
                    arg(a, 1).parse::<f32>().unwrap_or(0.0) as i32,
                );
                Value::Int(self.layout.hit(x, y).map_or(-1, |n| n as i64))
            }
            "computed" => {
                let Some(n) = node else {
                    return Value::Str(String::new());
                };
                let Some(s) = self.styles.get(n) else {
                    return Value::Str(String::new());
                };
                let color =
                    |c: u32| format!("rgb({}, {}, {})", (c >> 16) & 255, (c >> 8) & 255, c & 255);
                let r = self.layout.boxes.get(&n).copied().unwrap_or_default();
                Value::Str(match arg(a, 1) {
                    "display" => String::from(match s.display {
                        super::style::Display::None => "none",
                        super::style::Display::Inline => "inline",
                        super::style::Display::InlineBlock => "inline-block",
                        super::style::Display::Flex => "flex",
                        super::style::Display::Grid => "grid",
                        super::style::Display::Table => "table",
                        _ => "block",
                    }),
                    "visibility" => String::from(if s.visible { "visible" } else { "hidden" }),
                    "color" => color(s.color),
                    "background-color" => {
                        if s.background >> 24 == 0 {
                            String::from("rgba(0, 0, 0, 0)")
                        } else {
                            color(s.background)
                        }
                    }
                    "width" => format!("{}px", r.w),
                    "height" => format!("{}px", r.h),
                    "font-size" => format!("{}px", s.font_size),
                    "position" => String::from(match s.position {
                        super::style::Position::Static => "static",
                        super::style::Position::Relative => "relative",
                        super::style::Position::Absolute => "absolute",
                        super::style::Position::Fixed => "fixed",
                        super::style::Position::Sticky => "sticky",
                    }),
                    "opacity" => format!("{}", s.opacity),
                    _ => String::new(),
                })
            }
            "scrollto" => {
                self.st.scroll_to = node;
                Value::Undefined
            }
            "activate" => {
                if let Some(n) = node {
                    self.st.activated.push(n);
                }
                Value::Undefined
            }
            "submit" => {
                if let Some(n) = node {
                    self.st.submitted.push(n);
                }
                Value::Undefined
            }
            _ => {
                log(&format!("unknown native call {}", op));
                Value::Undefined
            }
        }
    }

    fn resolve_module(&mut self, base: &str, name: &str) -> Option<String> {
        let base = Url::parse_absolute(base.split('#').next().unwrap_or(base))
            .or_else(|| self.base.clone())?;
        base.join(name).map(|u| u.to_string())
    }

    fn load_module(&mut self, name: &str) -> Option<String> {
        if self.st.external >= MAX_SCRIPTS {
            return None;
        }
        self.st.external += 1;
        let u = Url::parse_absolute(name)?;
        fetch_text(&u)
    }
}
