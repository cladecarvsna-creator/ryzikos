//! Layout: turns the styled DOM into a display list of positioned boxes,
//! text and images. It covers normal flow (blocks, inline text with line
//! wrapping, inline-blocks), margins with collapsing between siblings,
//! padding, borders, widths and heights, floats (placed side by side),
//! flexbox, simple grids, tables, list markers, relative and absolute
//! positioning, and overflow clipping.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::dom::{Dom, NodeData, NodeId};
use super::style::{self, Align, Color, Display, Justify, Len, ListStyle, Position, Style};

/// A font face, picked from the style.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Face {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

impl Face {
    pub fn of(s: &Style) -> Face {
        Face {
            bold: s.bold,
            italic: s.italic,
            mono: s.mono,
        }
    }
}

/// Text measurements and image sizes, provided by whoever draws the page.
pub trait Metrics {
    /// Width of `text` in 1/16 pixels.
    fn width16(&self, face: Face, size: f32, text: &str) -> i32;
    /// Ascent and descent in pixels.
    fn ascent_descent(&self, face: Face, size: f32) -> (i32, i32);
    /// Size of a loaded image, by its `src`.
    /// The size of a downloaded image; None if it has not arrived yet.
    fn image_size(&self, src: &str) -> Option<(i32, i32)>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    fn offset(self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Text,
    Password,
    TextArea,
    Select,
    Checkbox,
    Radio,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// A placeholder that draws nothing.
    None,
    Fill {
        r: Rect,
        color: Color,
        radius: i32,
    },
    Border {
        r: Rect,
        widths: [i32; 4],
        colors: [Color; 4],
        radius: i32,
    },
    Text {
        x: i32,
        baseline: i32,
        w: i32,
        text: String,
        face: Face,
        size: f32,
        color: Color,
        underline: bool,
        strike: bool,
    },
    Image {
        r: Rect,
        src: String,
        /// Scale to cover the rectangle (backgrounds) instead of filling it.
        cover: bool,
    },
    /// A form field; the browser draws its value.
    Control {
        r: Rect,
        node: NodeId,
        kind: Control,
    },
    /// Draw only inside this rectangle until the matching `Unclip`.
    Clip(Rect),
    Unclip,
}

impl Item {
    fn translate(&mut self, dx: i32, dy: i32) {
        match self {
            Item::None | Item::Unclip => {}
            Item::Fill { r, .. }
            | Item::Border { r, .. }
            | Item::Image { r, .. }
            | Item::Control { r, .. }
            | Item::Clip(r) => *r = r.offset(dx, dy),
            Item::Text { x, baseline, .. } => {
                *x += dx;
                *baseline += dy;
            }
        }
    }
}

#[derive(Default)]
pub struct Layout {
    pub items: Vec<Item>,
    /// Boxes and text runs in paint order, for finding what was clicked.
    pub hits: Vec<(Rect, NodeId)>,
    /// Where each element ended up (its first box).
    pub boxes: BTreeMap<NodeId, Rect>,
    pub height: i32,
    /// The page background: the root's or the body's.
    pub canvas: Color,
}

impl Layout {
    /// The node drawn topmost at a point.
    pub fn hit(&self, x: i32, y: i32) -> Option<NodeId> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|&(_, n)| n)
    }
}

/// Lay out the document for a viewport `(width, height)`.
pub fn layout(dom: &Dom, styles: &[Rc<Style>], viewport: (i32, i32), m: &dyn Metrics) -> Layout {
    let mut e = Engine {
        dom,
        st: styles,
        m,
        vp: viewport,
        items: Vec::new(),
        hits: Vec::new(),
        boxes: Vec::new(),
        intrinsic: BTreeMap::new(),
        marker: None,
        abs: alloc::vec![Vec::new()],
        depth: 0,
    };
    let root = dom.html();
    let mut height = 0;
    if root != super::dom::DOCUMENT && e.style(root).display != Display::None {
        let mt = e.margin_top(root, viewport.0);
        let out = e.layout_box(
            root,
            0,
            mt,
            viewport.0,
            Some(viewport.1),
            Sizing::Fill,
            None,
        );
        height = out.border.bottom() + out.margin[2];
    }
    // absolutely positioned boxes with no positioned ancestor
    let pending = e.abs.pop().unwrap_or_default();
    e.place_absolutes(pending, Rect::new(0, 0, viewport.0, viewport.1.max(height)));
    let mut boxes = BTreeMap::new();
    for (n, r) in e.boxes {
        boxes.entry(n).or_insert(r);
    }
    let canvas = [root, dom.body()]
        .iter()
        .map(|&n| styles[n].background)
        .find(|&c| opaque(c))
        .unwrap_or(0xffff_ffff);
    Layout {
        canvas,
        items: e.items,
        hits: e.hits,
        boxes,
        height,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sizing {
    /// Fill the containing block (normal flow blocks).
    Fill,
    /// Shrink to fit the content (inline-blocks, floats, table cells).
    Shrink,
    /// Exactly this margin-box width (flex items, table cells).
    Exact(i32),
}

struct BoxOut {
    /// The border box.
    border: Rect,
    /// top, right, bottom, left
    margin: [i32; 4],
    /// Index range of this box's background items.
    bg: (usize, usize),
}

/// Laid out content that has not been placed yet: positioned at (0, 0).
#[derive(Default)]
struct Frag {
    items: Vec<Item>,
    hits: Vec<(Rect, NodeId)>,
    boxes: Vec<(NodeId, Rect)>,
}

impl Frag {
    /// Stretch this box's background to height `h`.
    fn stretch(&mut self, bg: (usize, usize), h: i32) {
        for i in bg.0..bg.1.min(self.items.len()) {
            match &mut self.items[i] {
                Item::Fill { r, .. } | Item::Border { r, .. } | Item::Image { r, .. } => {
                    r.h = r.h.max(h)
                }
                _ => {}
            }
        }
    }
}

/// One thing on a line.
enum Atom {
    Text {
        text: String,
        style: Rc<Style>,
        node: NodeId,
        underline: bool,
        strike: bool,
        open: Rc<Vec<NodeId>>,
    },
    Box {
        frag: Frag,
        w: i32,
        h: i32,
        /// Distance from the top to the baseline of its text.
        baseline: Option<i32>,
        middle: bool,
        open: Rc<Vec<NodeId>>,
    },
    Break,
    /// Space taken by an inline element's left or right padding, border and margin.
    Space(i32),
}

struct Piece {
    atom: usize,
    x16: i32,
    w16: i32,
    text: String,
}

struct Engine<'a> {
    dom: &'a Dom,
    st: &'a [Rc<Style>],
    m: &'a dyn Metrics,
    vp: (i32, i32),
    items: Vec<Item>,
    hits: Vec<(Rect, NodeId)>,
    boxes: Vec<(NodeId, Rect)>,
    intrinsic: BTreeMap<NodeId, (i32, i32)>,
    /// A list marker waiting for the first line of its item: text, style, x.
    marker: Option<(String, Rc<Style>, i32)>,
    /// Absolutely positioned boxes waiting for their containing block:
    /// node and static position.
    abs: Vec<Vec<(NodeId, i32, i32)>>,
    depth: u32,
}

const MAX_DEPTH: u32 = 120;

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\t' | '\r' | '\x0c')
}

fn opaque(c: Color) -> bool {
    c >> 24 != 0
}

impl Engine<'_> {
    fn style(&self, n: NodeId) -> &Rc<Style> {
        &self.st[n]
    }

    fn is_text(&self, n: NodeId) -> bool {
        matches!(self.dom.nodes[n].data, NodeData::Text(_))
    }

    fn text(&self, n: NodeId) -> &str {
        match &self.dom.nodes[n].data {
            NodeData::Text(t) => t,
            _ => "",
        }
    }

    /// Children in layout order: display:contents is looked through,
    /// display:none and comments are dropped.
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.collect_children(n, &mut out);
        out
    }

    fn collect_children(&self, n: NodeId, out: &mut Vec<NodeId>) {
        for &c in &self.dom.nodes[n].children {
            match &self.dom.nodes[c].data {
                NodeData::Text(_) => out.push(c),
                NodeData::Element(_) => match self.style(c).display {
                    Display::None => {}
                    Display::Contents => self.collect_children(c, out),
                    _ => out.push(c),
                },
                NodeData::Document => {}
            }
        }
    }

    fn is_replaced(&self, n: NodeId) -> bool {
        matches!(self.dom.tag(n), "img" | "input" | "textarea" | "select")
    }

    /// Whether a node takes part in inline layout.
    fn is_inline_level(&self, n: NodeId) -> bool {
        if self.is_text(n) {
            return true;
        }
        let s = self.style(n);
        s.display.is_inline_level() || (s.display == Display::Inline)
    }

    fn capture<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> (R, Frag) {
        let (i, h, b) = (self.items.len(), self.hits.len(), self.boxes.len());
        let r = f(self);
        let frag = Frag {
            items: self.items.split_off(i),
            hits: self.hits.split_off(h),
            boxes: self.boxes.split_off(b),
        };
        (r, frag)
    }

    fn place(&mut self, frag: Frag, dx: i32, dy: i32) {
        for mut it in frag.items {
            it.translate(dx, dy);
            self.items.push(it);
        }
        for (r, n) in frag.hits {
            self.hits.push((r.offset(dx, dy), n));
        }
        for (n, r) in frag.boxes {
            self.boxes.push((n, r.offset(dx, dy)));
        }
    }

    fn translate_since(&mut self, from: (usize, usize, usize), dx: i32, dy: i32) {
        for it in &mut self.items[from.0..] {
            it.translate(dx, dy);
        }
        for (r, _) in &mut self.hits[from.1..] {
            *r = r.offset(dx, dy);
        }
        for (_, r) in &mut self.boxes[from.2..] {
            *r = r.offset(dx, dy);
        }
    }

    // ---- box model ----------------------------------------------------------------

    fn margin_top(&self, n: NodeId, cb_w: i32) -> i32 {
        self.style(n).margin[0].or0(cb_w)
    }

    fn edges(s: &Style, cb_w: i32) -> ([i32; 4], [i32; 4]) {
        let p = [
            s.padding[0].or0(cb_w).max(0),
            s.padding[1].or0(cb_w).max(0),
            s.padding[2].or0(cb_w).max(0),
            s.padding[3].or0(cb_w).max(0),
        ];
        let b = [
            s.border[0] as i32,
            s.border[1] as i32,
            s.border[2] as i32,
            s.border[3] as i32,
        ];
        (p, b)
    }

    /// Convert a specified width or height to a border-box size.
    fn to_border(s: &Style, v: i32, extra: i32) -> i32 {
        if s.border_box {
            v.max(extra)
        } else {
            v + extra
        }
    }

    /// Lay out an element as a block box whose margin box starts at (x, y).
    #[allow(clippy::too_many_arguments)]
    fn layout_box(
        &mut self,
        n: NodeId,
        x: i32,
        y: i32,
        cb_w: i32,
        cb_h: Option<i32>,
        sizing: Sizing,
        forced_h: Option<i32>,
    ) -> BoxOut {
        crate::fiber::pause_if_slice_used();
        let s = self.style(n).clone();
        let tag = self.dom.tag(n);
        let (p, b) = Self::edges(&s, cb_w);
        let h_extra = p[1] + p[3] + b[1] + b[3];
        let v_extra = p[0] + p[2] + b[0] + b[2];
        let mut margin = [
            s.margin[0].or0(cb_w),
            s.margin[1].or0(cb_w),
            s.margin[2].or0(cb_w),
            s.margin[3].or0(cb_w),
        ];
        let replaced = self.replaced_size(n, &s, cb_w);
        let spec_w = s
            .width
            .resolve(cb_w)
            .map(|w| Self::to_border(&s, w, h_extra));
        let clamp_w = |w: i32| {
            let mut w = w;
            if let Some(mx) = s.max_width.resolve(cb_w) {
                w = w.min(Self::to_border(&s, mx, h_extra));
            }
            if let Some(mn) = s.min_width.resolve(cb_w) {
                w = w.max(Self::to_border(&s, mn, h_extra));
            }
            w.max(h_extra)
        };
        let is_table = s.display == Display::Table || tag == "table";
        let sizing = if is_table && sizing == Sizing::Fill && spec_w.is_none() {
            Sizing::Shrink
        } else {
            sizing
        };
        let avail = cb_w - margin[1] - margin[3];
        let mut bw = match sizing {
            Sizing::Exact(w) => w - margin[1] - margin[3],
            _ if spec_w.is_some() => spec_w.unwrap(),
            _ if replaced.is_some() => replaced.unwrap().0 + h_extra,
            Sizing::Fill => avail,
            Sizing::Shrink => {
                let (mn, mx) = self.intrinsic(n);
                let (mn, mx) = (mn - margin[1] - margin[3], mx - margin[1] - margin[3]);
                avail.min(mx).max(mn)
            }
        };
        if !matches!(sizing, Sizing::Exact(_)) {
            bw = clamp_w(bw);
        }
        bw = bw.max(0);
        // auto margins centre a block with a width
        if matches!(sizing, Sizing::Fill | Sizing::Shrink)
            && (spec_w.is_some() || s.max_width.resolve(cb_w).is_some() || is_table)
        {
            let rest = cb_w - bw;
            match (s.margin[1].is_auto(), s.margin[3].is_auto()) {
                (true, true) => {
                    margin[3] = (rest / 2).max(0);
                    margin[1] = rest - margin[3];
                }
                (false, true) => margin[3] = (rest - margin[1]).max(margin[3]),
                _ => {}
            }
        }
        let bx = x + margin[3];
        let by = y + margin[0];
        let cx = bx + b[3] + p[3];
        let cy = by + b[0] + p[0];
        let cw = (bw - h_extra).max(0);
        let spec_h = s
            .height
            .resolve(cb_h.unwrap_or(0))
            .filter(|_| !matches!(s.height, Len::Val { pct, .. } if pct != 0.0 && cb_h.is_none()))
            .map(|h| Self::to_border(&s, h, v_extra));
        let content_cb_h = forced_h.or(spec_h).map(|h| (h - v_extra).max(0));

        let start = (self.items.len(), self.hits.len(), self.boxes.len());
        // background, border and clip slots, filled in once the height is known
        self.items.push(Item::None);
        self.items.push(Item::None);
        self.items.push(Item::None);
        self.items.push(Item::None);
        let bg_end = self.items.len();
        self.hits.push((Rect::default(), n));
        let positioned = s.position != Position::Static;
        if positioned {
            self.abs.push(Vec::new());
        }

        self.depth += 1;
        let ch = if self.depth > MAX_DEPTH {
            0
        } else if let Some((_, h)) = replaced {
            self.replaced_content(n, &s, cx, cy, cw, h);
            h
        } else {
            if s.display == Display::ListItem && s.list_style != ListStyle::None {
                let marker = self.marker_text(n, &s);
                self.marker = Some((marker, s.clone(), cx));
            }
            let h = match s.display {
                Display::Flex | Display::InlineFlex => self.flex(n, &s, cx, cy, cw, content_cb_h),
                Display::Grid => self.grid(n, &s, cx, cy, cw),
                _ if is_table => self.table(n, &s, cx, cy, cw),
                _ => self.flow(n, cx, cy, cw, content_cb_h),
            };
            if s.display == Display::ListItem {
                self.marker = None;
            }
            h
        };
        self.depth -= 1;

        let mut bh = forced_h.or(spec_h).unwrap_or(ch + v_extra);
        if let Some(mx) = s
            .max_height
            .resolve(cb_h.unwrap_or(0))
            .filter(|_| !s.max_height.is_auto())
        {
            if !matches!(s.max_height, Len::Val { pct, .. } if pct != 0.0 && cb_h.is_none()) {
                bh = bh.min(Self::to_border(&s, mx, v_extra));
            }
        }
        if let Some(mn) = s.min_height.resolve(cb_h.unwrap_or(self.vp.1)) {
            bh = bh.max(Self::to_border(&s, mn, v_extra));
        }
        bh = bh.max(v_extra);
        let border = Rect::new(bx, by, bw, bh);
        let radius = (s.radius as i32).min(bw / 2).min(bh / 2);
        if s.visible {
            if opaque(s.background) {
                self.items[start.0] = Item::Fill {
                    r: border,
                    color: s.background,
                    radius,
                };
            }
            if let Some(src) = &s.background_image {
                if bw >= 32 && bh >= 32 {
                    self.items[start.0 + 1] = Item::Image {
                        r: border,
                        src: src.clone(),
                        cover: true,
                    };
                }
            }
            if b.iter().any(|&w| w > 0) {
                self.items[start.0 + 2] = Item::Border {
                    r: border,
                    widths: b,
                    colors: s.border_color,
                    radius,
                };
            }
        }
        if s.clip && replaced.is_none() {
            self.items[start.0 + 3] = Item::Clip(border);
            self.items.push(Item::Unclip);
        }
        self.hits[start.1] = (border, n);
        self.boxes.push((n, border));
        if positioned {
            let pending = self.abs.pop().unwrap_or_default();
            let padding_box = Rect::new(bx + b[3], by + b[0], bw - b[1] - b[3], bh - b[0] - b[2]);
            self.place_absolutes(pending, padding_box);
        }
        if s.position == Position::Relative || s.position == Position::Sticky {
            let dx = match (s.inset[3], s.inset[1]) {
                (l, _) if !l.is_auto() => l.or0(cb_w),
                (_, r) if !r.is_auto() => -r.or0(cb_w),
                _ => 0,
            };
            let dy = match (s.inset[0], s.inset[2]) {
                (t, _) if !t.is_auto() => t.or0(cb_h.unwrap_or(0)),
                (_, b) if !b.is_auto() => -b.or0(cb_h.unwrap_or(0)),
                _ => 0,
            };
            if dx != 0 || dy != 0 {
                self.translate_since(start, dx, dy);
            }
        }
        BoxOut {
            border,
            margin,
            bg: (start.0, bg_end),
        }
    }

    /// Lay out absolutely positioned boxes against a containing block.
    fn place_absolutes(&mut self, pending: Vec<(NodeId, i32, i32)>, cb: Rect) {
        for (n, sx, sy) in pending {
            let s = self.style(n).clone();
            let l = (!s.inset[3].is_auto()).then(|| s.inset[3].or0(cb.w));
            let r = (!s.inset[1].is_auto()).then(|| s.inset[1].or0(cb.w));
            let t = (!s.inset[0].is_auto()).then(|| s.inset[0].or0(cb.h));
            let b = (!s.inset[2].is_auto()).then(|| s.inset[2].or0(cb.h));
            let sizing = match (l, r, s.width.is_auto()) {
                (Some(l), Some(r), true) => Sizing::Exact((cb.w - l - r).max(0)),
                _ => Sizing::Shrink,
            };
            let forced_h = match (t, b, s.height.is_auto()) {
                (Some(t), Some(b), true) => Some((cb.h - t - b).max(0)),
                _ => None,
            };
            let (out, frag) =
                self.capture(|e| e.layout_box(n, 0, 0, cb.w, Some(cb.h), sizing, forced_h));
            let (w, h) = (
                out.border.w + out.margin[1] + out.margin[3],
                out.border.h + out.margin[0] + out.margin[2],
            );
            let x = match (l, r) {
                (Some(l), _) => cb.x + l,
                (None, Some(r)) => cb.right() - r - w,
                _ => sx,
            };
            let y = match (t, b) {
                (Some(t), _) => cb.y + t,
                (None, Some(b)) => cb.bottom() - b - h,
                _ => sy,
            };
            self.place(frag, x, y);
        }
    }

    // ---- replaced elements and form fields ----------------------------------------

    /// Content size of images and form fields.
    fn replaced_size(&self, n: NodeId, s: &Style, cb_w: i32) -> Option<(i32, i32)> {
        let tag = self.dom.tag(n);
        let line = s.line_px().max(s.font_size as i32 + 4);
        let (p, b) = Self::edges(s, cb_w);
        let h_extra = p[1] + p[3] + b[1] + b[3];
        let v_extra = p[0] + p[2] + b[0] + b[2];
        let spec_w = s
            .width
            .resolve(cb_w)
            .map(|w| if s.border_box { w - h_extra } else { w });
        let spec_h = s
            .height
            .resolve(0)
            .filter(|_| !matches!(s.height, Len::Val { pct, .. } if pct != 0.0))
            .map(|h| if s.border_box { h - v_extra } else { h });
        match tag {
            "img" => {
                // the picture's own size, only asked for when needed: the
                // page is laid out again when an image whose size was
                // missing arrives
                let natural = if spec_w.is_none() || spec_h.is_none() {
                    self.dom
                        .attr(n, "src")
                        .and_then(|src| self.m.image_size(src))
                        .map(|(w, h)| (w.max(1), h.max(1)))
                } else {
                    None
                };
                let mut max_w = s.max_width.resolve(cb_w);
                if let Some(mw) = max_w.as_mut() {
                    *mw = (*mw - if s.border_box { h_extra } else { 0 }).max(0);
                }
                let (w, h) = match (spec_w, spec_h, natural) {
                    (Some(w), Some(h), _) => (w, h),
                    (Some(w), None, Some((nw, nh))) => (w, w * nh / nw),
                    (None, Some(h), Some((nw, nh))) => (h * nw / nh, h),
                    (None, None, Some((nw, nh))) => (nw, nh),
                    (Some(w), None, None) => (w, 0),
                    (None, Some(h), None) => (0, h),
                    (None, None, None) => (0, 0),
                };
                // max-width: 100% keeps big images inside the page
                if let Some(mw) = max_w {
                    if w > mw && w > 0 {
                        return Some((mw, if spec_h.is_some() { h } else { h * mw / w }));
                    }
                }
                Some((w.max(0), h.max(0)))
            }
            "input" => {
                let t = self
                    .dom
                    .attr(n, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" | "radio" => Some((spec_w.unwrap_or(13), spec_h.unwrap_or(13))),
                    "submit" | "button" | "reset" => {
                        let label = self.button_label(n);
                        let w = self.m.width16(Face::of(s), s.font_size, &label) / 16;
                        Some((spec_w.unwrap_or(w), spec_h.unwrap_or(line)))
                    }
                    "image" => Some((spec_w.unwrap_or(24), spec_h.unwrap_or(24))),
                    _ => {
                        let size = self
                            .dom
                            .attr(n, "size")
                            .and_then(|v| v.trim().parse::<i32>().ok());
                        let w = size.map_or(180, |c| c * (s.font_size as i32 / 2 + 1));
                        Some((spec_w.unwrap_or(w).max(10), spec_h.unwrap_or(line)))
                    }
                }
            }
            "textarea" => {
                let cols = self
                    .dom
                    .attr(n, "cols")
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or(30);
                let rows = self
                    .dom
                    .attr(n, "rows")
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or(3);
                Some((
                    spec_w.unwrap_or(cols * (s.font_size as i32 / 2 + 1)),
                    spec_h.unwrap_or(rows * s.line_px()),
                ))
            }
            "select" => {
                let widest = self
                    .dom
                    .descendants(n)
                    .into_iter()
                    .filter(|&o| self.dom.tag(o) == "option")
                    .map(|o| {
                        self.m
                            .width16(Face::of(s), s.font_size, self.dom.text_content(o).trim())
                            / 16
                    })
                    .max()
                    .unwrap_or(40);
                Some((spec_w.unwrap_or(widest + 28), spec_h.unwrap_or(line)))
            }
            _ => None,
        }
    }

    fn button_label(&self, n: NodeId) -> String {
        match self.dom.attr(n, "value") {
            Some(v) => v.to_string(),
            None => match self
                .dom
                .attr(n, "type")
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "reset" => String::from("Reset"),
                "submit" => String::from("Submit"),
                _ => String::new(),
            },
        }
    }

    fn replaced_content(&mut self, n: NodeId, s: &Rc<Style>, cx: i32, cy: i32, cw: i32, ch: i32) {
        if !s.visible {
            return;
        }
        let r = Rect::new(cx, cy, cw, ch);
        match self.dom.tag(n) {
            "img" => {
                if let Some(src) = self.dom.attr(n, "src") {
                    if cw > 0 && ch > 0 {
                        self.items.push(Item::Image {
                            r,
                            src: src.to_string(),
                            cover: false,
                        });
                    }
                }
            }
            "input" => {
                let t = self
                    .dom
                    .attr(n, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase();
                let kind = match t.as_str() {
                    "checkbox" => Control::Checkbox,
                    "radio" => Control::Radio,
                    "password" => Control::Password,
                    "submit" | "button" | "reset" => {
                        let label = self.button_label(n);
                        self.text_line(&label, s, cx, cy, cw, ch, Align::Center);
                        return;
                    }
                    "image" => return,
                    _ => Control::Text,
                };
                self.items.push(Item::Control { r, node: n, kind });
            }
            "textarea" => self.items.push(Item::Control {
                r,
                node: n,
                kind: Control::TextArea,
            }),
            "select" => self.items.push(Item::Control {
                r,
                node: n,
                kind: Control::Select,
            }),
            _ => {}
        }
    }

    /// One line of text in a box, for button labels.
    #[allow(clippy::too_many_arguments)]
    fn text_line(&mut self, text: &str, s: &Style, x: i32, y: i32, w: i32, h: i32, align: Align) {
        let face = Face::of(s);
        let tw = self.m.width16(face, s.font_size, text) / 16;
        let (asc, desc) = self.m.ascent_descent(face, s.font_size);
        let tx = match align {
            Align::Center => x + (w - tw) / 2,
            Align::Right => x + w - tw,
            _ => x,
        };
        self.items.push(Item::Text {
            x: tx,
            baseline: y + (h - asc - desc) / 2 + asc,
            w: tw,
            text: text.to_string(),
            face,
            size: s.font_size,
            color: s.color,
            underline: false,
            strike: false,
        });
    }

    fn marker_text(&self, n: NodeId, s: &Style) -> String {
        let index = || {
            let parent = self.dom.nodes[n].parent;
            let start = parent
                .and_then(|p| self.dom.attr(p, "start"))
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(1);
            if let Some(v) = self
                .dom
                .attr(n, "value")
                .and_then(|v| v.trim().parse::<i32>().ok())
            {
                return v;
            }
            let mut i = start;
            if let Some(p) = parent {
                for &c in &self.dom.nodes[p].children {
                    if c == n {
                        break;
                    }
                    if self.dom.tag(c) == "li" {
                        i += 1;
                    }
                }
            }
            i
        };
        match s.list_style {
            ListStyle::None => String::new(),
            ListStyle::Disc => String::from("•"),
            ListStyle::Circle => String::from("◦"),
            ListStyle::Square => String::from("▪"),
            ListStyle::Decimal => alloc::format!("{}.", index()),
            ListStyle::LowerAlpha | ListStyle::UpperAlpha => {
                let i = (index() - 1).rem_euclid(26) as u8;
                let c = if s.list_style == ListStyle::LowerAlpha {
                    b'a' + i
                } else {
                    b'A' + i
                };
                alloc::format!("{}.", c as char)
            }
            ListStyle::LowerRoman | ListStyle::UpperRoman => {
                let r = roman(index());
                alloc::format!(
                    "{}.",
                    if s.list_style == ListStyle::LowerRoman {
                        r.to_lowercase()
                    } else {
                        r
                    }
                )
            }
        }
    }

    // ---- intrinsic widths -----------------------------------------------------------

    /// The narrowest and the widest a node's margin box can usefully be.
    fn intrinsic(&mut self, n: NodeId) -> (i32, i32) {
        if let Some(&v) = self.intrinsic.get(&n) {
            return v;
        }
        self.depth += 1;
        let v = if self.depth > MAX_DEPTH {
            (0, 0)
        } else {
            self.compute_intrinsic(n)
        };
        self.depth -= 1;
        self.intrinsic.insert(n, v);
        v
    }

    fn text_intrinsic(&self, n: NodeId) -> (i32, i32) {
        let s = self.style(n).clone();
        let face = Face::of(&s);
        let text = style::transform_text(self.text(n), s.transform);
        if s.pre {
            let max = text
                .split('\n')
                .map(|l| self.m.width16(face, s.font_size, &l.replace('\t', "    ")))
                .max()
                .unwrap_or(0);
            return ((if s.nowrap { max } else { 0 }) / 16, max / 16);
        }
        let mut max = 0;
        let mut min = 0;
        let space = self.m.width16(face, s.font_size, " ");
        let mut first = true;
        for word in text.split(is_space).filter(|w| !w.is_empty()) {
            let w = self.m.width16(face, s.font_size, word);
            min = min.max(w);
            max += w + if first { 0 } else { space };
            first = false;
        }
        if s.nowrap {
            min = max;
        }
        ((min + 15) / 16, (max + 15) / 16)
    }

    fn compute_intrinsic(&mut self, n: NodeId) -> (i32, i32) {
        if self.is_text(n) {
            return self.text_intrinsic(n);
        }
        let s = self.style(n).clone();
        if s.display == Display::None {
            return (0, 0);
        }
        let (p, b) = Self::edges(&s, 0);
        let h_extra = p[1] + p[3] + b[1] + b[3];
        let margins = s.margin[1].or0(0).max(0) + s.margin[3].or0(0).max(0);
        if let Some((w, _)) = self.replaced_size(n, &s, 0) {
            if !matches!(s.width, Len::Val { pct, .. } if pct != 0.0) {
                let w = w + h_extra + margins;
                return (w, w);
            }
            return (margins + h_extra, w + h_extra + margins);
        }
        if let Len::Val { px, pct } = s.width {
            if pct == 0.0 {
                let w = Self::to_border(&s, px as i32, h_extra) + margins;
                return (w, w);
            }
        }
        let kids = self.children(n);
        let (mut min, mut max);
        match s.display {
            Display::Flex | Display::InlineFlex if !s.flex_column => {
                min = 0;
                max = 0;
                let gap = s.gap as i32;
                let mut count = 0;
                for &c in &kids {
                    if !self.is_text(c) && self.style(c).position == Position::Absolute {
                        continue;
                    }
                    let (a, b) = self.intrinsic(c);
                    if self.is_text(c) && b == 0 {
                        continue;
                    }
                    if s.flex_wrap {
                        min = min.max(a);
                    } else {
                        min += a;
                    }
                    max += b;
                    count += 1;
                }
                if count > 1 {
                    max += gap * (count - 1);
                    if !s.flex_wrap {
                        min += gap * (count - 1);
                    }
                }
            }
            Display::Grid if s.grid_columns > 1 => {
                let cols = s.grid_columns as i32;
                let (mut a, mut b) = (0, 0);
                for &c in &kids {
                    let (x, y) = self.intrinsic(c);
                    a = a.max(x);
                    b = b.max(y);
                }
                let gaps = s.gap as i32 * (cols - 1);
                min = a * cols + gaps;
                max = b * cols + gaps;
            }
            Display::Table => {
                let (a, b) = self.table_intrinsic(n, &s);
                min = a;
                max = b;
            }
            _ => {
                // lines of inline content and blocks
                min = 0;
                max = 0;
                let mut line_max = 0;
                self.flow_intrinsic(&kids, &mut min, &mut max, &mut line_max);
                max = max.max(line_max);
            }
        }
        let clamp = |v: i32| {
            let mut v = v;
            if let Len::Val { px, pct } = s.max_width {
                if pct == 0.0 {
                    v = v.min(Self::to_border(&s, px as i32, h_extra) - h_extra);
                }
            }
            if let Len::Val { px, pct } = s.min_width {
                if pct == 0.0 {
                    v = v.max(px as i32);
                }
            }
            v
        };
        (
            clamp(min) + h_extra + margins,
            clamp(max).max(clamp(min)) + h_extra + margins,
        )
    }

    fn flow_intrinsic(&mut self, kids: &[NodeId], min: &mut i32, max: &mut i32, line: &mut i32) {
        for &c in kids {
            if self.is_text(c) {
                let (a, b) = self.intrinsic(c);
                *min = (*min).max(a);
                *line += b;
                continue;
            }
            let cs = self.style(c).clone();
            if cs.position == Position::Absolute {
                continue;
            }
            if cs.display == Display::Inline && !self.is_replaced(c) {
                if self.dom.tag(c) == "br" {
                    *max = (*max).max(*line);
                    *line = 0;
                    continue;
                }
                let (p, b) = Self::edges(&cs, 0);
                *line += p[1] + p[3] + b[1] + b[3] + cs.margin[1].or0(0) + cs.margin[3].or0(0);
                let sub = self.children(c);
                self.flow_intrinsic(&sub, min, max, line);
            } else if cs.display.is_inline_level() || cs.float_left || cs.float_right {
                let (a, b) = self.intrinsic(c);
                *min = (*min).max(a);
                *line += b;
            } else {
                *max = (*max).max(*line);
                *line = 0;
                let (a, b) = self.intrinsic(c);
                *min = (*min).max(a);
                *max = (*max).max(b);
            }
        }
    }

    // ---- normal flow ----------------------------------------------------------------

    /// Lay out the children of `n` in a content box. Returns the content height.
    fn flow(&mut self, n: NodeId, cx: i32, cy: i32, cw: i32, cb_h: Option<i32>) -> i32 {
        let kids = self.children(n);
        let container = self.style(n).clone();
        let mut y = cy;
        let mut prev_mb = 0;
        let mut run: Vec<NodeId> = Vec::new();
        // floats: the row they sit on
        let mut float_row: Option<(i32, i32, i32, i32)> = None; // top, left x, right x, height
        let mut i = 0;
        while i < kids.len() {
            let c = kids[i];
            i += 1;
            if !self.is_text(c) {
                let cs = self.style(c).clone();
                if cs.position == Position::Absolute {
                    if let Some(top) = self.abs.last_mut() {
                        top.push((c, cx, y + prev_mb));
                    }
                    continue;
                }
                if cs.float_left || cs.float_right {
                    if !run.is_empty() {
                        y = self.flush_inline(&mut run, &container, cx, y + prev_mb, cw);
                        prev_mb = 0;
                    }
                    let (top, lx, rx, h) = float_row.unwrap_or((y + prev_mb, cx, cx + cw, 0));
                    let (out, frag) =
                        self.capture(|e| e.layout_box(c, 0, 0, cw, cb_h, Sizing::Shrink, None));
                    let w = out.border.w + out.margin[1] + out.margin[3];
                    let fh = out.border.h + out.margin[0] + out.margin[2];
                    let (top, lx, rx, h) = if rx - lx < w && (lx > cx || rx < cx + cw) {
                        // no room: start a new row below
                        (top + h, cx, cx + cw, 0)
                    } else {
                        (top, lx, rx, h)
                    };
                    if cs.float_right {
                        self.place(frag, rx - w, top);
                        float_row = Some((top, lx, rx - w, h.max(fh)));
                    } else {
                        self.place(frag, lx, top);
                        float_row = Some((top, lx + w, rx, h.max(fh)));
                    }
                    continue;
                }
                if !self.is_inline_level(c) {
                    if !run.is_empty() {
                        y = self.flush_inline(&mut run, &container, cx, y + prev_mb, cw);
                        prev_mb = 0;
                    }
                    let clear = float_row.take();
                    if let Some((top, _, _, h)) = clear {
                        let bottom = top + h;
                        if bottom > y {
                            y = bottom;
                            prev_mb = 0;
                        }
                    }
                    let mt = self.margin_top(c, cw);
                    let top = y + prev_mb.max(mt) - mt;
                    let out = self.layout_box(c, cx, top, cw, cb_h, Sizing::Fill, None);
                    y = out.border.bottom();
                    prev_mb = out.margin[2];
                    continue;
                }
            }
            if let Some((top, _, _, h)) = float_row.take() {
                if !run.is_empty() {
                    y = self.flush_inline(&mut run, &container, cx, y + prev_mb, cw);
                    prev_mb = 0;
                }
                y = y.max(top + h);
            }
            run.push(c);
        }
        if !run.is_empty() {
            y = self.flush_inline(&mut run, &container, cx, y + prev_mb, cw);
            prev_mb = 0;
        }
        if let Some((top, _, _, h)) = float_row {
            y = y.max(top + h);
        }
        y + prev_mb.max(0) - cy
    }

    /// Lay out a run of inline content; returns the new y. Empty runs of
    /// white space take no room.
    fn flush_inline(
        &mut self,
        run: &mut Vec<NodeId>,
        container: &Rc<Style>,
        x: i32,
        y: i32,
        w: i32,
    ) -> i32 {
        let nodes = core::mem::take(run);
        let only_space = nodes
            .iter()
            .all(|&n| self.is_text(n) && self.text(n).chars().all(is_space));
        if only_space && !container.pre {
            return y;
        }
        y + self.inline(&nodes, container, x, y, w)
    }

    // ---- inline layout -------------------------------------------------------------

    fn collect_atoms(
        &mut self,
        nodes: &[NodeId],
        width: i32,
        atoms: &mut Vec<Atom>,
        open: &Rc<Vec<NodeId>>,
        deco: (bool, bool),
        last_space: &mut bool,
    ) {
        for &n in nodes {
            if self.is_text(n) {
                let s = self.style(n).clone();
                let raw = self.text(n);
                let text = if s.pre {
                    raw.replace('\t', "    ").replace("\r\n", "\n")
                } else {
                    let mut t = String::with_capacity(raw.len());
                    for ch in raw.chars() {
                        if is_space(ch) {
                            if !*last_space {
                                t.push(' ');
                                *last_space = true;
                            }
                        } else {
                            t.push(ch);
                            *last_space = false;
                        }
                    }
                    t
                };
                if text.is_empty() {
                    continue;
                }
                let text = style::transform_text(&text, s.transform);
                if s.pre {
                    let mut first = true;
                    for line in text.split('\n') {
                        if !first {
                            atoms.push(Atom::Break);
                        }
                        first = false;
                        if !line.is_empty() {
                            atoms.push(Atom::Text {
                                text: line.to_string(),
                                style: s.clone(),
                                node: self.dom.nodes[n].parent.unwrap_or(n),
                                underline: deco.0 || s.underline,
                                strike: deco.1 || s.line_through,
                                open: open.clone(),
                            });
                        }
                    }
                    *last_space = false;
                } else {
                    atoms.push(Atom::Text {
                        text,
                        style: s.clone(),
                        node: self.dom.nodes[n].parent.unwrap_or(n),
                        underline: deco.0 || s.underline,
                        strike: deco.1 || s.line_through,
                        open: open.clone(),
                    });
                }
                continue;
            }
            let s = self.style(n).clone();
            if s.position == Position::Absolute {
                // placed at the start of the line it would be on
                continue;
            }
            let tag = self.dom.tag(n);
            if tag == "br" {
                atoms.push(Atom::Break);
                *last_space = true;
                continue;
            }
            if tag == "wbr" {
                continue;
            }
            if s.display == Display::Inline && !self.is_replaced(n) {
                let (p, b) = Self::edges(&s, width);
                let left = p[3] + b[3] + s.margin[3].or0(width);
                let right = p[1] + b[1] + s.margin[1].or0(width);
                let decorated = s.visible && (opaque(s.background) || b.iter().any(|&w| w > 0));
                let open = if decorated {
                    let mut v = (**open).clone();
                    v.push(n);
                    Rc::new(v)
                } else {
                    open.clone()
                };
                if left != 0 {
                    atoms.push(Atom::Space(left));
                }
                let kids = self.children(n);
                self.collect_atoms(
                    &kids,
                    width,
                    atoms,
                    &open,
                    (deco.0 || s.underline, deco.1 || s.line_through),
                    last_space,
                );
                if right != 0 {
                    atoms.push(Atom::Space(right));
                }
                continue;
            }
            // an atomic box: inline-block, image, field, or a block inside inline
            let block_in_inline = !s.display.is_inline_level() && !self.is_replaced(n);
            let sizing = if block_in_inline {
                Sizing::Fill
            } else {
                Sizing::Shrink
            };
            if block_in_inline {
                atoms.push(Atom::Break);
            }
            let (out, frag) = self.capture(|e| e.layout_box(n, 0, 0, width, None, sizing, None));
            let h = out.border.h + out.margin[0] + out.margin[2];
            // an inline-block sits on the baseline of its last line of text
            let baseline = if self.is_replaced(n) || block_in_inline {
                None
            } else {
                frag.items.iter().rev().find_map(|i| match i {
                    Item::Text { baseline, .. } => Some(*baseline),
                    _ => None,
                })
            };
            atoms.push(Atom::Box {
                frag,
                w: out.border.w + out.margin[1] + out.margin[3],
                h,
                baseline: baseline.filter(|&b| b > 0 && b <= h),
                middle: s.vertical_middle,
                open: open.clone(),
            });
            *last_space = false;
            if block_in_inline {
                atoms.push(Atom::Break);
                *last_space = true;
            }
        }
    }

    /// Lay out inline content into lines. Returns the height.
    fn inline(&mut self, nodes: &[NodeId], container: &Rc<Style>, x: i32, y: i32, w: i32) -> i32 {
        let mut atoms = Vec::new();
        let mut last_space = true;
        self.collect_atoms(
            nodes,
            w,
            &mut atoms,
            &Rc::new(Vec::new()),
            (false, false),
            &mut last_space,
        );
        let w16 = w.max(1) * 16;
        let mut lines: Vec<Vec<Piece>> = alloc::vec![Vec::new()];
        let mut pen = 0; // 1/16 px
        let mut atoms_boxes: Vec<Option<Frag>> = Vec::with_capacity(atoms.len());
        let has_content = |line: &Vec<Piece>| !line.is_empty();
        for (ai, atom) in atoms.iter_mut().enumerate() {
            match atom {
                Atom::Break => {
                    lines.push(Vec::new());
                    pen = 0;
                    atoms_boxes.push(None);
                }
                Atom::Space(px) => {
                    pen += *px * 16;
                    atoms_boxes.push(None);
                }
                Atom::Box { frag, w: bw, .. } => {
                    let bw16 = *bw * 16;
                    if pen + bw16 > w16 && has_content(lines.last().unwrap()) {
                        lines.push(Vec::new());
                        pen = 0;
                    }
                    lines.last_mut().unwrap().push(Piece {
                        atom: ai,
                        x16: pen,
                        w16: bw16,
                        text: String::new(),
                    });
                    pen += bw16;
                    atoms_boxes.push(Some(core::mem::take(frag)));
                }
                Atom::Text { text, style, .. } => {
                    atoms_boxes.push(None);
                    let face = Face::of(style);
                    let size = style.font_size;
                    let nowrap = style.nowrap || style.pre && style.nowrap;
                    let tokens: Vec<&str> = if nowrap {
                        alloc::vec![text.as_str()]
                    } else {
                        split_words(text)
                    };
                    for tok in tokens {
                        let trimmed = tok.trim_end_matches(' ');
                        let tw = self.m.width16(face, size, trimmed);
                        let full = if trimmed.len() == tok.len() {
                            tw
                        } else {
                            self.m.width16(face, size, tok)
                        };
                        let line_empty = !has_content(lines.last().unwrap());
                        if pen + tw > w16 && !line_empty && !nowrap {
                            lines.push(Vec::new());
                            pen = 0;
                        }
                        if lines.last().unwrap().is_empty() && tok.trim().is_empty() && !style.pre {
                            continue; // no spaces at the start of a line
                        }
                        // a word longer than the line: break it anywhere
                        if tw > w16 && !nowrap && pen == 0 {
                            let mut chunk = String::new();
                            let mut cw = 0;
                            for ch in tok.chars() {
                                let chw = self.m.width16(face, size, ch.encode_utf8(&mut [0; 4]));
                                if cw + chw > w16 && !chunk.is_empty() {
                                    self.push_piece(&mut lines, ai, pen, cw, &chunk);
                                    lines.push(Vec::new());
                                    pen = 0;
                                    chunk.clear();
                                    cw = 0;
                                }
                                chunk.push(ch);
                                cw += chw;
                            }
                            self.push_piece(&mut lines, ai, pen, cw, &chunk);
                            pen += cw;
                            continue;
                        }
                        self.push_piece(&mut lines, ai, pen, full, tok);
                        pen += full;
                    }
                }
            }
        }
        // place the lines
        let mut top = y;
        let strut = {
            let face = Face::of(container);
            let (a, d) = self.m.ascent_descent(face, container.font_size);
            let lh = container.line_px();
            let asc = a + (lh - a - d) / 2;
            (asc, lh - asc)
        };
        for line in lines.iter_mut() {
            // trailing spaces do not count
            if let Some(last) = line.last_mut() {
                if let Atom::Text { style, .. } = &atoms[last.atom] {
                    let t = last.text.trim_end_matches(' ');
                    if t.len() != last.text.len() {
                        let face = Face::of(style);
                        last.w16 = self.m.width16(face, style.font_size, t);
                        let l = t.len();
                        last.text.truncate(l);
                    }
                }
            }
            line.retain(|p| {
                p.w16 > 0 || !p.text.is_empty() || matches!(atoms[p.atom], Atom::Box { .. })
            });
            if line.is_empty() {
                continue;
            }
            let mut asc = 0;
            let mut desc = 0;
            let mut has_text = false;
            for p in line.iter() {
                match &atoms[p.atom] {
                    Atom::Text { style, .. } => {
                        let (a, d) = self.m.ascent_descent(Face::of(style), style.font_size);
                        let lh = style.line_px();
                        let pa = a + (lh - a - d) / 2;
                        asc = asc.max(pa);
                        desc = desc.max(lh - pa);
                        has_text = true;
                    }
                    Atom::Box {
                        h,
                        middle,
                        baseline,
                        ..
                    } => {
                        if *middle {
                            let half = h / 2 + strut.0 / 4;
                            asc = asc.max(half);
                            desc = desc.max(h - half);
                        } else if let Some(b) = baseline {
                            asc = asc.max(*b);
                            desc = desc.max(h - b);
                        } else {
                            asc = asc.max(*h);
                        }
                    }
                    _ => {}
                }
            }
            if has_text {
                asc = asc.max(strut.0);
                desc = desc.max(strut.1);
            }
            let line_h = asc + desc;
            let baseline = top + asc;
            let right = line.iter().map(|p| p.x16 + p.w16).max().unwrap_or(0);
            let shift = match container.text_align {
                Align::Center => ((w16 - right) / 2).max(0),
                Align::Right => (w16 - right).max(0),
                _ => 0,
            };
            // list marker on the first line
            if let Some((marker, ms, mx)) = self.marker.take() {
                let face = Face::of(&ms);
                let mw = self.m.width16(face, ms.font_size, &marker) / 16;
                self.items.push(Item::Text {
                    x: mx - mw - 8,
                    baseline,
                    w: mw,
                    text: marker,
                    face,
                    size: ms.font_size,
                    color: ms.color,
                    underline: false,
                    strike: false,
                });
            }
            // backgrounds of inline elements
            let mut spans: Vec<(NodeId, i32, i32)> = Vec::new();
            for p in line.iter() {
                let open = match &atoms[p.atom] {
                    Atom::Text { open, .. } | Atom::Box { open, .. } => open.clone(),
                    _ => continue,
                };
                for &o in open.iter() {
                    let (x0, x1) = (p.x16, p.x16 + p.w16);
                    match spans.iter_mut().find(|s| s.0 == o) {
                        Some(s) => {
                            s.1 = s.1.min(x0);
                            s.2 = s.2.max(x1);
                        }
                        None => spans.push((o, x0, x1)),
                    }
                }
            }
            for (o, x0, x1) in spans {
                let s = self.style(o).clone();
                let (p, b) = Self::edges(&s, w);
                let (a, d) = self.m.ascent_descent(Face::of(&s), s.font_size);
                let r = Rect::new(
                    x + (x0 + shift) / 16 - p[3] - b[3],
                    baseline - a - p[0] - b[0],
                    (x1 - x0) / 16 + p[1] + p[3] + b[1] + b[3],
                    a + d + p[0] + p[2] + b[0] + b[2],
                );
                let radius = s.radius as i32;
                if opaque(s.background) {
                    self.items.push(Item::Fill {
                        r,
                        color: s.background,
                        radius,
                    });
                }
                if b.iter().any(|&w| w > 0) {
                    self.items.push(Item::Border {
                        r,
                        widths: b,
                        colors: s.border_color,
                        radius,
                    });
                }
            }
            for p in line.iter() {
                let px = x + (p.x16 + shift + 8) / 16;
                match &atoms[p.atom] {
                    Atom::Text {
                        style,
                        node,
                        underline,
                        strike,
                        ..
                    } => {
                        let pw = (p.w16 + 8) / 16;
                        let r = Rect::new(px, top, pw, line_h);
                        self.hits.push((r, *node));
                        self.boxes.push((*node, r));
                        if style.visible && !p.text.is_empty() {
                            self.items.push(Item::Text {
                                x: px,
                                baseline,
                                w: pw,
                                text: p.text.clone(),
                                face: Face::of(style),
                                size: style.font_size,
                                color: style.color,
                                underline: *underline,
                                strike: *strike,
                            });
                        }
                    }
                    Atom::Box {
                        h,
                        middle,
                        baseline: b,
                        ..
                    } => {
                        let by = if *middle {
                            baseline - (h / 2 + strut.0 / 4)
                        } else if let Some(b) = b {
                            baseline - b
                        } else {
                            baseline - h
                        };
                        if let Some(frag) = atoms_boxes[p.atom].take() {
                            self.place(frag, px, by);
                        }
                    }
                    _ => {}
                }
            }
            top += line_h;
        }
        top - y
    }

    fn push_piece(&self, lines: &mut [Vec<Piece>], atom: usize, pen: i32, w16: i32, text: &str) {
        let line = lines.last_mut().unwrap();
        if let Some(last) = line.last_mut() {
            if last.atom == atom && last.x16 + last.w16 == pen {
                last.text.push_str(text);
                last.w16 += w16;
                return;
            }
        }
        line.push(Piece {
            atom,
            x16: pen,
            w16,
            text: text.to_string(),
        });
    }

    // ---- flexbox ----------------------------------------------------------------------

    /// Flex items: elements, and runs of text wrapped in anonymous boxes.
    fn flex_items(&self, n: NodeId) -> Vec<NodeId> {
        let mut items: Vec<NodeId> = self
            .children(n)
            .into_iter()
            .filter(|&c| {
                if self.is_text(c) {
                    !self.text(c).chars().all(is_space)
                } else {
                    true
                }
            })
            .collect();
        items.sort_by_key(|&c| {
            if self.is_text(c) {
                0
            } else {
                self.style(c).order
            }
        });
        items
    }

    /// Lay out one flex or grid item at (0, 0) with a margin-box width.
    fn item_frag(
        &mut self,
        c: NodeId,
        container: &Rc<Style>,
        w: i32,
        cb_h: Option<i32>,
        forced_h: Option<i32>,
    ) -> (BoxOut, Frag) {
        if self.is_text(c) {
            let (h, frag) = self.capture(|e| e.inline(&[c], container, 0, 0, w));
            return (
                BoxOut {
                    border: Rect::new(0, 0, w, h),
                    margin: [0; 4],
                    bg: (0, 0),
                },
                frag,
            );
        }
        self.capture(|e| e.layout_box(c, 0, 0, w, cb_h, Sizing::Exact(w), forced_h))
    }

    fn flex(
        &mut self,
        n: NodeId,
        s: &Rc<Style>,
        cx: i32,
        cy: i32,
        cw: i32,
        cb_h: Option<i32>,
    ) -> i32 {
        let all = self.flex_items(n);
        let mut items = Vec::new();
        for c in all {
            if !self.is_text(c) && self.style(c).position == Position::Absolute {
                if let Some(top) = self.abs.last_mut() {
                    top.push((c, cx, cy));
                }
            } else {
                items.push(c);
            }
        }
        let gap = s.gap as i32;
        if s.flex_column {
            return self.flex_column(&items, s, cx, cy, cw, cb_h, gap);
        }
        // hypothetical main sizes
        struct FI {
            node: NodeId,
            base: i32,
            min: i32,
            max: i32,
            grow: f32,
            shrink: f32,
            size: i32,
        }
        let mut fis = Vec::new();
        for &c in &items {
            let (min_c, max_c) = self.intrinsic(c);
            let (grow, shrink, base, min, max) = if self.is_text(c) {
                (0.0, 1.0, max_c, min_c, i32::MAX)
            } else {
                let cs = self.style(c).clone();
                let (p, b) = Self::edges(&cs, cw);
                let extra = p[1] + p[3] + b[1] + b[3];
                let margins = cs.margin[1].or0(cw) + cs.margin[3].or0(cw);
                let outer = |v: i32| Self::to_border(&cs, v, extra) + margins;
                let base = match (cs.flex_basis.resolve(cw), cs.width.resolve(cw)) {
                    (Some(b), _) if !cs.flex_basis.is_auto() => outer(b),
                    (_, Some(w)) => outer(w),
                    _ => max_c,
                };
                let min = match cs.min_width.resolve(cw) {
                    Some(m) => outer(m),
                    None if cs.clip => margins + extra,
                    None => min_c.min(base),
                };
                let max = cs.max_width.resolve(cw).map_or(i32::MAX, outer);
                (cs.flex_grow, cs.flex_shrink, base, min, max)
            };
            fis.push(FI {
                node: c,
                base,
                min,
                max,
                grow,
                shrink,
                size: base.clamp(min, max.max(min)),
            });
        }
        // break into lines
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        let mut used = 0;
        for (i, fi) in fis.iter().enumerate() {
            let add = fi.size + if i > start { gap } else { 0 };
            if s.flex_wrap && i > start && used + add > cw {
                lines.push((start, i));
                start = i;
                used = fi.size;
            } else {
                used += add;
            }
        }
        lines.push((start, fis.len()));
        let mut y = cy;
        let definite_single = cb_h.filter(|_| lines.len() == 1);
        for (li, &(a, b)) in lines.iter().enumerate() {
            if a == b {
                continue;
            }
            let count = (b - a) as i32;
            let gaps = gap * (count - 1);
            let total: i32 = fis[a..b].iter().map(|f| f.size).sum::<i32>() + gaps;
            let free = cw - total;
            if free > 0 {
                let grow: f32 = fis[a..b].iter().map(|f| f.grow).sum();
                if grow > 0.0 {
                    let mut left = free;
                    for f in &mut fis[a..b] {
                        let add = ((free as f32) * f.grow / grow) as i32;
                        let new = (f.size + add).min(f.max.max(f.min));
                        left -= new - f.size;
                        f.size = new;
                    }
                    let _ = left;
                }
            } else if free < 0 {
                // shrink in rounds, respecting minimum sizes
                let mut over = -free;
                for _ in 0..4 {
                    let weight: f32 = fis[a..b]
                        .iter()
                        .filter(|f| f.size > f.min)
                        .map(|f| f.shrink * f.base.max(1) as f32)
                        .sum();
                    if weight <= 0.0 || over <= 0 {
                        break;
                    }
                    let mut taken = 0;
                    for f in &mut fis[a..b] {
                        if f.size <= f.min {
                            continue;
                        }
                        let cut =
                            ((over as f32) * f.shrink * f.base.max(1) as f32 / weight) as i32 + 1;
                        let new = (f.size - cut).max(f.min);
                        taken += f.size - new;
                        f.size = new;
                    }
                    over -= taken;
                }
            }
            // lay out the items to learn their heights
            let mut frags = Vec::new();
            let mut line_h = 0;
            for f in &fis[a..b] {
                let (out, frag) = self.item_frag(f.node, s, f.size, None, None);
                let h = out.border.h + out.margin[0] + out.margin[2];
                line_h = line_h.max(h);
                frags.push((out, frag, h));
            }
            if let Some(h) = definite_single {
                line_h = line_h.max(h);
            }
            let used: i32 = fis[a..b].iter().map(|f| f.size).sum::<i32>() + gaps;
            let free = (cw - used).max(0);
            let (mut x, spacing) = match s.justify {
                Justify::Start => (0, 0),
                Justify::End => (free, 0),
                Justify::Center => (free / 2, 0),
                Justify::SpaceBetween if count > 1 => (0, free / (count - 1)),
                Justify::SpaceBetween => (0, 0),
                Justify::SpaceAround => (free / count / 2, free / count),
                Justify::SpaceEvenly => (free / (count + 1), free / (count + 1)),
            };
            for (k, (out, mut frag, h)) in frags.into_iter().enumerate() {
                let f = &fis[a + k];
                let stretch =
                    s.align_stretch && !self.is_text(f.node) && self.style(f.node).height.is_auto();
                let dy = if s.align_center {
                    (line_h - h) / 2
                } else if s.align_end {
                    line_h - h
                } else {
                    0
                };
                if stretch && h < line_h {
                    frag.stretch(out.bg, out.border.h + line_h - h);
                }
                self.place(frag, cx + x, y + dy);
                x += f.size + gap + spacing;
            }
            y += line_h;
            if li + 1 < lines.len() {
                y += gap;
            }
        }
        y - cy
    }

    #[allow(clippy::too_many_arguments)]
    fn flex_column(
        &mut self,
        items: &[NodeId],
        s: &Rc<Style>,
        cx: i32,
        cy: i32,
        cw: i32,
        cb_h: Option<i32>,
        gap: i32,
    ) -> i32 {
        let mut frags = Vec::new();
        let mut total = 0;
        for (i, &c) in items.iter().enumerate() {
            let shrink = !self.is_text(c) && (!s.align_stretch || !self.style(c).width.is_auto());
            let (out, frag) = if shrink {
                self.capture(|e| e.layout_box(c, 0, 0, cw, None, Sizing::Shrink, None))
            } else {
                self.item_frag(c, s, cw, None, None)
            };
            let w = out.border.w + out.margin[1] + out.margin[3];
            let h = out.border.h + out.margin[0] + out.margin[2];
            total += h + if i > 0 { gap } else { 0 };
            frags.push((c, w, h, frag, out));
        }
        let free = cb_h.map_or(0, |h| (h - total).max(0));
        let grow: f32 = items
            .iter()
            .filter(|&&c| !self.is_text(c))
            .map(|&c| self.style(c).flex_grow)
            .sum();
        let mut y = cy
            + if grow > 0.0 {
                0
            } else {
                match s.justify {
                    Justify::Center => free / 2,
                    Justify::End => free,
                    _ => 0,
                }
            };
        let count = frags.len();
        for (i, (c, w, h, frag, out)) in frags.into_iter().enumerate() {
            let x = if s.align_center {
                (cw - w) / 2
            } else if s.align_end {
                cw - w
            } else {
                0
            };
            let extra = if grow > 0.0 && !self.is_text(c) {
                (free as f32 * self.style(c).flex_grow / grow) as i32
            } else {
                0
            };
            if extra > 0 {
                // grow: lay out again with the taller height
                let (out2, frag2) = self.capture(|e| {
                    e.layout_box(
                        c,
                        0,
                        0,
                        cw,
                        None,
                        Sizing::Exact(w),
                        Some(out.border.h + extra),
                    )
                });
                let _ = out2;
                self.place(frag2, cx + x, y);
            } else {
                self.place(frag, cx + x, y);
            }
            y += h + extra;
            if i + 1 < count {
                y += gap;
            }
        }
        (y - cy).max(cb_h.filter(|_| grow > 0.0).unwrap_or(0))
    }

    // ---- grid ----------------------------------------------------------------------

    fn grid(&mut self, n: NodeId, s: &Rc<Style>, cx: i32, cy: i32, cw: i32) -> i32 {
        let items = self.flex_items(n);
        let gap = s.gap as i32;
        let cols = if s.grid_columns == 0 {
            1
        } else {
            s.grid_columns as i32
        };
        let col_w = ((cw - gap * (cols - 1)) / cols).max(0);
        let mut y = cy;
        for row in items.chunks(cols as usize) {
            let mut frags = Vec::new();
            let mut row_h = 0;
            for &c in row {
                let (out, frag) = self.item_frag(c, s, col_w, None, None);
                let h = out.border.h + out.margin[0] + out.margin[2];
                row_h = row_h.max(h);
                frags.push((out, frag, h));
            }
            for (i, (out, mut frag, h)) in frags.into_iter().enumerate() {
                if s.align_stretch && h < row_h {
                    frag.stretch(out.bg, out.border.h + row_h - h);
                }
                let dy = if s.align_center { (row_h - h) / 2 } else { 0 };
                self.place(frag, cx + i as i32 * (col_w + gap), y + dy);
            }
            y += row_h + gap;
        }
        (y - cy - if items.is_empty() { 0 } else { gap }).max(0)
    }

    // ---- tables ------------------------------------------------------------------------

    /// Rows of a table, each a list of (cell, column span).
    fn table_rows(&self, n: NodeId) -> (Vec<Vec<(NodeId, usize)>>, Vec<NodeId>) {
        let mut rows = Vec::new();
        let mut captions = Vec::new();
        let add_row = |e: &Self, tr: NodeId, rows: &mut Vec<Vec<(NodeId, usize)>>| {
            let cells: Vec<(NodeId, usize)> = e
                .children(tr)
                .into_iter()
                .filter(|&c| !e.is_text(c))
                .map(|c| {
                    let span = e
                        .dom
                        .attr(c, "colspan")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 50);
                    (c, span)
                })
                .collect();
            rows.push(cells);
        };
        for c in self.children(n) {
            if self.is_text(c) {
                continue;
            }
            let d = self.style(c).display;
            match d {
                Display::TableRow => add_row(self, c, &mut rows),
                Display::TableCaption => captions.push(c),
                Display::TableRowGroup => {
                    for r in self.children(c) {
                        if !self.is_text(r) {
                            add_row(self, r, &mut rows);
                        }
                    }
                }
                Display::TableCell => rows.push(alloc::vec![(c, 1)]),
                _ => {
                    if self.dom.tag(c) == "tr" {
                        add_row(self, c, &mut rows);
                    } else {
                        captions.push(c);
                    }
                }
            }
        }
        (rows, captions)
    }

    fn column_widths(
        &mut self,
        rows: &[Vec<(NodeId, usize)>],
        cw: Option<i32>,
    ) -> (Vec<i32>, Vec<i32>, Vec<Option<i32>>) {
        let ncols = rows
            .iter()
            .map(|r| r.iter().map(|c| c.1).sum::<usize>())
            .max()
            .unwrap_or(0);
        let mut min = alloc::vec![0; ncols];
        let mut max = alloc::vec![0; ncols];
        let mut fixed: Vec<Option<i32>> = alloc::vec![None; ncols];
        for row in rows {
            let mut col = 0;
            for &(cell, span) in row {
                let (a, b) = self.intrinsic(cell);
                let cs = self.style(cell).clone();
                if span == 1 && col < ncols {
                    min[col] = min[col].max(a);
                    max[col] = max[col].max(b);
                    if let Len::Val { px, pct } = cs.width {
                        let v = match cw {
                            Some(cw) if pct != 0.0 => Some((pct * cw as f32 / 100.0) as i32),
                            _ if pct == 0.0 => {
                                let (p, bd) = Self::edges(&cs, 0);
                                Some(Self::to_border(&cs, px as i32, p[1] + p[3] + bd[1] + bd[3]))
                            }
                            _ => None,
                        };
                        if let Some(v) = v {
                            fixed[col] = Some(fixed[col].unwrap_or(0).max(v.max(a)));
                        }
                    }
                } else if col < ncols {
                    // spread a spanning cell over its columns
                    let end = (col + span).min(ncols);
                    let have_min: i32 = min[col..end].iter().sum();
                    let have_max: i32 = max[col..end].iter().sum();
                    let n = (end - col) as i32;
                    if a > have_min {
                        for m in &mut min[col..end] {
                            *m += (a - have_min) / n;
                        }
                    }
                    if b > have_max {
                        for m in &mut max[col..end] {
                            *m += (b - have_max) / n;
                        }
                    }
                }
                col += span;
            }
        }
        (min, max, fixed)
    }

    fn table_intrinsic(&mut self, n: NodeId, s: &Style) -> (i32, i32) {
        let (rows, _) = self.table_rows(n);
        let (min, max, fixed) = self.column_widths(&rows, None);
        let spacing = s.border_spacing as i32 * (min.len() as i32 + 1);
        let pick = |v: &Vec<i32>| -> i32 {
            v.iter()
                .zip(fixed.iter())
                .map(|(&a, f)| f.unwrap_or(a))
                .sum::<i32>()
                + spacing
        };
        (pick(&min), pick(&max))
    }

    fn table(&mut self, n: NodeId, s: &Rc<Style>, cx: i32, cy: i32, cw: i32) -> i32 {
        let (rows, captions) = self.table_rows(n);
        let mut y = cy;
        for cap in captions {
            let out = self.layout_box(cap, cx, y, cw, None, Sizing::Fill, None);
            y = out.border.bottom() + out.margin[2];
        }
        if rows.is_empty() {
            return y - cy;
        }
        let sp = s.border_spacing as i32;
        let (min, max, fixed) = self.column_widths(&rows, Some(cw));
        let ncols = min.len();
        let avail = (cw - sp * (ncols as i32 + 1)).max(0);
        // start from the minimum widths, then share out the rest
        let mut widths: Vec<i32> = (0..ncols).map(|i| fixed[i].unwrap_or(min[i])).collect();
        let used: i32 = widths.iter().sum();
        let mut extra = avail - used;
        if extra > 0 {
            let want: i32 = (0..ncols)
                .filter(|&i| fixed[i].is_none())
                .map(|i| (max[i] - min[i]).max(0))
                .sum();
            if want > 0 {
                let give = extra.min(want);
                for i in 0..ncols {
                    if fixed[i].is_none() {
                        widths[i] += give * (max[i] - min[i]).max(0) / want;
                    }
                }
                extra -= give;
            }
            // a table with a width: spread what is left
            if extra > 0 && !s.width.is_auto() {
                let flexible: Vec<usize> = (0..ncols).filter(|&i| fixed[i].is_none()).collect();
                let targets: Vec<usize> = if flexible.is_empty() {
                    (0..ncols).collect()
                } else {
                    flexible
                };
                let total: i32 = targets.iter().map(|&i| widths[i].max(1)).sum();
                for &i in &targets {
                    widths[i] += extra * widths[i].max(1) / total.max(1);
                }
            }
        }
        y += sp;
        for row in &rows {
            let mut frags = Vec::new();
            let mut row_h = 0;
            let mut col = 0;
            let mut x = sp;
            for &(cell, span) in row {
                let end = (col + span).min(ncols);
                let w: i32 =
                    widths[col.min(ncols)..end].iter().sum::<i32>() + sp * (span as i32 - 1);
                let (out, frag) =
                    self.capture(|e| e.layout_box(cell, 0, 0, w, None, Sizing::Exact(w), None));
                let h = out.border.h + out.margin[0] + out.margin[2];
                row_h = row_h.max(h);
                frags.push((cell, x, out, frag, h));
                x += w + sp;
                col = end;
            }
            // row background
            for (cell, x, out, mut frag, h) in frags {
                let middle = self.style(cell).vertical_middle;
                if h < row_h {
                    if middle {
                        // move the content down, keep the background at the top
                        let dy = (row_h - h) / 2;
                        for (i, it) in frag.items.iter_mut().enumerate() {
                            if i >= out.bg.1 {
                                it.translate(0, dy);
                            }
                        }
                    }
                    frag.stretch(out.bg, out.border.h + row_h - h);
                }
                self.place(frag, cx + x, y);
            }
            y += row_h + sp;
        }
        y - cy
    }
}

/// Split text into words, each keeping the spaces after it.
fn split_words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            out.push(&text[start..i]);
            start = i;
        } else {
            i += 1;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn roman(mut n: i32) -> String {
    let mut s = String::new();
    if n <= 0 {
        return alloc::format!("{}", n);
    }
    for (v, r) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}
