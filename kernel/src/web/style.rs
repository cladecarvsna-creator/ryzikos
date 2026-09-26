//! Computed styles: the cascade (user agent sheet, page sheets, style
//! attributes), inheritance, custom properties and value parsing.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::css::{self, parse_f32, split_top, Decl, Media, Selector, Stylesheet};
use super::dom::{Dom, NodeData, NodeId};

pub type Color = u32;

/// Colours are 0xAARRGGBB; alpha 0 is transparent.
pub const TRANSPARENT: Color = 0;
pub const BLACK: Color = 0xff00_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    ListItem,
    Table,
    TableRow,
    TableCell,
    TableRowGroup,
    TableCaption,
    Contents,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline | Display::InlineBlock | Display::InlineFlex
        )
    }
}

/// A length: `px` pixels plus `pct` percent of the containing block.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Len {
    Auto,
    Val { px: f32, pct: f32 },
}

impl Len {
    pub const ZERO: Len = Len::Val { px: 0.0, pct: 0.0 };

    pub fn px(v: f32) -> Len {
        Len::Val { px: v, pct: 0.0 }
    }

    pub fn resolve(self, base: i32) -> Option<i32> {
        match self {
            Len::Auto => None,
            Len::Val { px, pct } => Some((px + pct * base as f32 / 100.0) as i32),
        }
    }

    pub fn or0(self, base: i32) -> i32 {
        self.resolve(base).unwrap_or(0)
    }

    pub fn is_auto(self) -> bool {
        matches!(self, Len::Auto)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub color: Color,
    pub background: Color,
    pub background_image: Option<String>,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    pub line_through: bool,
    pub text_align: Align,
    pub pre: bool,
    pub nowrap: bool,
    pub line_height: Option<f32>,
    pub transform: Transform,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border: [f32; 4],
    pub border_color: [Color; 4],
    pub radius: f32,
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub max_width: Len,
    pub min_height: Len,
    pub max_height: Len,
    pub border_box: bool,
    pub list_style: ListStyle,
    pub visible: bool,
    pub flex_column: bool,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub align_center: bool,
    pub gap: f32,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Len,
    pub order: i32,
    pub grid_columns: usize,
    pub position: Position,
    /// top, right, bottom, left
    pub inset: [Len; 4],
    /// align-items is stretch (or normal)
    pub align_stretch: bool,
    pub align_end: bool,
    pub border_spacing: f32,
    pub float_left: bool,
    pub float_right: bool,
    pub clip: bool,
    pub opacity: f32,
    pub vertical_middle: bool,
    pub vars: Rc<BTreeMap<String, String>>,
}

impl Style {
    pub fn root() -> Style {
        Style {
            display: Display::Block,
            color: BLACK,
            background: TRANSPARENT,
            background_image: None,
            font_size: 16.0,
            bold: false,
            italic: false,
            mono: false,
            underline: false,
            line_through: false,
            text_align: Align::Left,
            pre: false,
            nowrap: false,
            line_height: None,
            transform: Transform::None,
            margin: [Len::ZERO; 4],
            padding: [Len::ZERO; 4],
            border: [0.0; 4],
            border_color: [BLACK; 4],
            radius: 0.0,
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            max_width: Len::Auto,
            min_height: Len::Auto,
            max_height: Len::Auto,
            border_box: false,
            list_style: ListStyle::Disc,
            visible: true,
            flex_column: false,
            flex_wrap: false,
            justify: Justify::Start,
            align_center: false,
            gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Len::Auto,
            order: 0,
            grid_columns: 0,
            position: Position::Static,
            inset: [Len::Auto; 4],
            align_stretch: true,
            align_end: false,
            border_spacing: 0.0,
            float_left: false,
            float_right: false,
            clip: false,
            opacity: 1.0,
            vertical_middle: false,
            vars: Rc::new(BTreeMap::new()),
        }
    }

    /// A child's starting point: inherited properties kept, the rest reset.
    fn inherit(&self) -> Style {
        let mut s = Style::root();
        s.color = self.color;
        s.font_size = self.font_size;
        s.bold = self.bold;
        s.italic = self.italic;
        s.mono = self.mono;
        s.text_align = self.text_align;
        s.pre = self.pre;
        s.nowrap = self.nowrap;
        s.line_height = self.line_height;
        s.transform = self.transform;
        s.list_style = self.list_style;
        s.visible = self.visible;
        s.border_spacing = self.border_spacing;
        s.vars = self.vars.clone();
        s.display = Display::Inline;
        s
    }

    pub fn line_px(&self) -> i32 {
        let lh = self.line_height.unwrap_or(1.3);
        // unitless (<= 4) is a multiple of the font size
        (if lh <= 4.0 { lh * self.font_size } else { lh }) as i32
    }
}

/// The browser's own style sheet.
const USER_AGENT_CSS: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption, footer, form,
header, hr, legend, listing, main, p, plaintext, pre, xmp, search, article, aside, h1, h2,
h3, h4, h5, h6, hgroup, nav, section, dl, dt, dd, ol, ul, menu, dir, details, summary,
fieldset, optgroup, frameset, frame { display: block }
head, script, style, title, meta, link, base, template, noscript, datalist, param, source,
track, area, map, svg, iframe, object, embed, video, audio, canvas, dialog:not([open]),
option, [hidden], input[type=hidden] { display: none }
body { margin: 8px }
p, blockquote, figure, dl, ul, ol, menu, dir, pre, xmp, listing, plaintext { margin: 1em 0 }
blockquote, figure { margin-left: 40px; margin-right: 40px }
h1 { font-size: 2em; margin: .67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: .83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { font-size: 1em; margin: 1.33em 0; font-weight: bold }
h5 { font-size: .83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: .67em; margin: 2.33em 0; font-weight: bold }
ul, menu, dir, ol { padding-left: 40px }
ul { list-style-type: disc }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle; margin: 0 }
ol ol, ul ol { margin: 0 }
li { display: list-item }
dd { margin-left: 40px }
dt, th, b, strong { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
code, kbd, samp, tt, pre, xmp, listing, plaintext { font-family: monospace }
pre, xmp, listing, plaintext, textarea { white-space: pre }
a:link, a[href] { color: #1a0dab; text-decoration: underline }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small, sub, sup { font-size: .83em }
big { font-size: 1.17em }
mark { background-color: #ff0; color: #000 }
center { text-align: center }
hr { border: 1px solid #ccc; margin: .5em 0; height: 0 }
table { display: table; border-spacing: 2px }
caption { display: table-caption; text-align: center }
tr { display: table-row }
thead, tbody, tfoot { display: table-row-group }
td, th { display: table-cell; padding: 1px; vertical-align: middle }
th { text-align: center }
img, input, button, select, textarea, progress, meter { display: inline-block }
input, select, textarea, button { font-size: 14px }
button, input[type=submit], input[type=button], input[type=reset] { padding: 3px 10px; border: 1px solid #aaa; background-color: #efefef; border-radius: 4px }
input, textarea, select { padding: 3px 5px; border: 1px solid #999; background-color: #fff; border-radius: 3px }
fieldset { border: 1px solid #aaa; padding: .35em .75em .6em; margin: 0 2px }
summary { font-weight: bold }
ruby rt { display: none }
"#;

pub struct Styler {
    sheets: Vec<(Rc<Stylesheet>, u8)>,
    /// Rules indexed by the key of their rightmost compound.
    by_id: BTreeMap<String, Vec<(usize, usize, usize)>>,
    by_class: BTreeMap<String, Vec<(usize, usize, usize)>>,
    by_tag: BTreeMap<String, Vec<(usize, usize, usize)>>,
    other: Vec<(usize, usize, usize)>,
    pub viewport: (i32, i32),
}

impl Styler {
    /// `author` holds the page's style sheets in document order.
    pub fn new(author: Vec<Rc<Stylesheet>>, viewport: (i32, i32)) -> Styler {
        let media = Media {
            width: viewport.0,
            height: viewport.1,
        };
        let mut sheets = alloc::vec![(Rc::new(css::parse_stylesheet(USER_AGENT_CSS, &media)), 0u8)];
        for s in author {
            sheets.push((s, 1));
        }
        let mut st = Styler {
            sheets,
            by_id: BTreeMap::new(),
            by_class: BTreeMap::new(),
            by_tag: BTreeMap::new(),
            other: Vec::new(),
            viewport,
        };
        for (si, (sheet, _)) in st.sheets.iter().enumerate() {
            for (ri, rule) in sheet.rules.iter().enumerate() {
                for (sel_i, sel) in rule.selectors.iter().enumerate() {
                    let key = (si, ri, sel_i);
                    let subject = &sel.parts[0].0;
                    if let Some(id) = &subject.id {
                        st.by_id.entry(id.clone()).or_default().push(key);
                    } else if let Some(c) = subject.classes.first() {
                        st.by_class.entry(c.clone()).or_default().push(key);
                    } else if let Some(t) = &subject.tag {
                        st.by_tag.entry(t.clone()).or_default().push(key);
                    } else {
                        st.other.push(key);
                    }
                }
            }
        }
        st
    }

    fn selector(&self, key: (usize, usize, usize)) -> &Selector {
        &self.sheets[key.0].0.rules[key.1].selectors[key.2]
    }

    /// Compute styles for every node. Text nodes get their parent's.
    pub fn compute(&self, dom: &Dom) -> Vec<Rc<Style>> {
        let root = Rc::new(Style::root());
        let mut out: Vec<Rc<Style>> = alloc::vec![root.clone(); dom.nodes.len()];
        let mut stack = alloc::vec![(super::dom::DOCUMENT, root)];
        while let Some((node, parent_style)) = stack.pop() {
            crate::fiber::pause_if_slice_used();
            let style = match &dom.nodes[node].data {
                NodeData::Element(_) => Rc::new(self.style_for(dom, node, &parent_style)),
                _ => parent_style.clone(),
            };
            out[node] = style.clone();
            for &c in dom.nodes[node].children.iter().rev() {
                stack.push((c, style.clone()));
            }
        }
        out
    }

    fn style_for(&self, dom: &Dom, node: NodeId, parent: &Style) -> Style {
        let el = dom.element(node).unwrap();
        // collect candidate rules
        let mut keys: Vec<(usize, usize, usize)> = Vec::new();
        if let Some(id) = el.attr("id") {
            if let Some(v) = self.by_id.get(id) {
                keys.extend(v);
            }
        }
        if let Some(classes) = el.attr("class") {
            for c in classes.split_ascii_whitespace() {
                if let Some(v) = self.by_class.get(c) {
                    keys.extend(v);
                }
            }
        }
        if let Some(v) = self.by_tag.get(&el.tag) {
            keys.extend(v);
        }
        keys.extend(&self.other);
        // (important, origin, specificity, order) sorts the cascade
        let mut matched: Vec<(u32, u32, &Decl)> = Vec::new();
        keys.sort_unstable();
        keys.dedup();
        let mut best: BTreeMap<(usize, usize), u32> = BTreeMap::new();
        for key in keys {
            let sel = self.selector(key);
            if css::matches(dom, node, sel) {
                let e = best.entry((key.0, key.1)).or_insert(0);
                *e = (*e).max(sel.specificity + 1);
            }
        }
        for ((si, ri), spec) in best {
            let origin = self.sheets[si].1 as u32;
            let order = ((si << 20) + ri) as u32;
            for d in &self.sheets[si].0.rules[ri].decls {
                let weight =
                    (d.important as u32) << 31 | origin << 30 | (spec - 1).min((1 << 30) - 1);
                matched.push((weight, order, d));
            }
        }
        let inline = el
            .attr("style")
            .map(css::parse_declarations)
            .unwrap_or_default();
        for d in &inline {
            let weight = (d.important as u32) << 31 | 1 << 30 | ((1 << 30) - 1);
            matched.push((weight, u32::MAX, d));
        }
        matched.sort_by_key(|&(w, o, _)| (w, o));

        let mut s = parent.inherit();
        // custom properties first, so var() sees them
        let mut vars: Option<BTreeMap<String, String>> = None;
        for (_, _, d) in &matched {
            if d.name.starts_with("--") {
                vars.get_or_insert_with(|| (*s.vars).clone())
                    .insert(d.name.clone(), d.value.clone());
            }
        }
        if let Some(v) = vars {
            s.vars = Rc::new(v);
        }
        let mut hinted = false;
        for (w, _, d) in &matched {
            // presentational attributes sit between the browser's and the page's rules
            if !hinted && w >> 30 != 0 {
                hinted = true;
                self.presentational_hints(dom, node, &mut s, parent);
            }
            if d.name.starts_with("--") {
                continue;
            }
            let value = if d.value.contains("var(") {
                substitute_vars(&d.value, &s.vars)
            } else {
                d.value.clone()
            };
            apply(&mut s, parent, &d.name, value.trim(), self.viewport);
        }
        if !hinted {
            self.presentational_hints(dom, node, &mut s, parent);
        }
        fixups(&mut s, parent, dom.tag(node));
        s
    }

    fn presentational_hints(&self, dom: &Dom, node: NodeId, s: &mut Style, parent: &Style) {
        let el = dom.element(node).unwrap();
        let tag = el.tag.as_str();
        if let Some(c) = el.attr("bgcolor").and_then(parse_color) {
            s.background = c;
        }
        if tag == "font" {
            if let Some(c) = el.attr("color").and_then(parse_color) {
                s.color = c;
            }
            if let Some(size) = el.attr("size").and_then(|v| v.trim().parse::<i32>().ok()) {
                s.font_size = match size {
                    i32::MIN..=1 => 10.0,
                    2 => 13.0,
                    3 => 16.0,
                    4 => 18.0,
                    5 => 24.0,
                    6 => 32.0,
                    _ => 48.0,
                };
            }
        }
        if tag == "body" {
            if let Some(c) = el.attr("text").and_then(parse_color) {
                s.color = c;
            }
        }
        for (attr, target) in [("width", 0), ("height", 1)] {
            if let Some(v) = el.attr(attr) {
                if matches!(
                    tag,
                    "img"
                        | "table"
                        | "td"
                        | "th"
                        | "input"
                        | "col"
                        | "iframe"
                        | "hr"
                        | "video"
                        | "canvas"
                ) {
                    let v = v.trim();
                    let len = if let Some(p) = v.strip_suffix('%') {
                        parse_f32(p).map(|p| Len::Val { px: 0.0, pct: p })
                    } else {
                        parse_f32(v.trim_end_matches("px")).map(Len::px)
                    };
                    if let Some(len) = len {
                        if target == 0 {
                            s.width = len;
                        } else {
                            s.height = len;
                        }
                    }
                }
            }
        }
        if let Some(a) = el.attr("align") {
            match a.to_ascii_lowercase().as_str() {
                "center" | "middle" => {
                    if tag == "table" {
                        s.margin[1] = Len::Auto;
                        s.margin[3] = Len::Auto;
                    } else if tag != "img" {
                        s.text_align = Align::Center;
                    }
                }
                "right" if tag != "img" && tag != "table" => s.text_align = Align::Right,
                _ => {}
            }
        }
        if tag == "td" || tag == "th" {
            // cellpadding and border from the table
            let mut t = dom.nodes[node].parent;
            while let Some(p) = t {
                if dom.tag(p) == "table" {
                    if let Some(cp) = dom
                        .attr(p, "cellpadding")
                        .and_then(|v| v.trim().parse::<f32>().ok())
                    {
                        s.padding = [Len::px(cp); 4];
                    }
                    if dom.attr(p, "border").is_some_and(|b| b.trim() != "0") {
                        s.border = [1.0; 4];
                        s.border_color = [0xff80_8080; 4];
                    }
                    break;
                }
                t = dom.nodes[p].parent;
            }
        }
        if tag == "table" {
            if let Some(cs) = el
                .attr("cellspacing")
                .and_then(|b| b.trim().parse::<f32>().ok())
            {
                s.border_spacing = cs;
            }
            if let Some(b) = el.attr("border").and_then(|b| b.trim().parse::<f32>().ok()) {
                s.border = [b; 4];
                s.border_color = [0xff80_8080; 4];
            }
        }
        let _ = parent;
    }
}

fn substitute_vars(value: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::from(value);
    for _ in 0..8 {
        let Some(pos) = out.find("var(") else {
            break;
        };
        // find the matching ')'
        let bytes = out.as_bytes();
        let mut depth = 0;
        let mut end = out.len();
        for (i, &b) in bytes.iter().enumerate().skip(pos + 3) {
            match b {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &out[pos + 4..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let replacement = vars
            .get(name)
            .map(|v| v.as_str())
            .or(fallback)
            .unwrap_or("")
            .to_string();
        out = alloc::format!(
            "{}{}{}",
            &out[..pos],
            replacement,
            out.get(end + 1..).unwrap_or("")
        );
    }
    out
}

/// Parse a length. `em` is the font size ems refer to.
pub fn parse_len(v: &str, em: f32, viewport: (i32, i32)) -> Option<Len> {
    let v = v.trim().to_ascii_lowercase();
    if v == "auto" || v == "fit-content" || v == "max-content" || v == "min-content" || v == "none"
    {
        return Some(Len::Auto);
    }
    if v == "0" {
        return Some(Len::ZERO);
    }
    for f in ["calc(", "min(", "max(", "clamp("] {
        if let Some(inner) = v.strip_prefix(f) {
            let inner = inner.strip_suffix(')').unwrap_or(inner);
            if f == "calc(" {
                return calc(inner, em, viewport);
            }
            // min/max/clamp: take the plain-pixel argument if there is one
            let args = split_top(inner, ',');
            let lens: Vec<Len> = args
                .iter()
                .filter_map(|a| parse_len(a, em, viewport))
                .collect();
            let pick = match f {
                "clamp(" => lens.get(1).copied(),
                "min(" => lens
                    .iter()
                    .copied()
                    .filter(|l| matches!(l, Len::Val { pct, .. } if *pct == 0.0))
                    .min_by(|a, b| a.or0(0).cmp(&b.or0(0)))
                    .or(lens.first().copied()),
                _ => lens
                    .iter()
                    .copied()
                    .filter(|l| matches!(l, Len::Val { pct, .. } if *pct == 0.0))
                    .max_by(|a, b| a.or0(0).cmp(&b.or0(0)))
                    .or(lens.first().copied()),
            };
            return pick;
        }
    }
    let num_end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(v.len());
    // "1em" would stop at 'e'; retry without exponent support
    let (num, unit) = match parse_f32(&v[..num_end]) {
        Some(n) => (n, &v[num_end..]),
        None => {
            let e = v
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
                .unwrap_or(v.len());
            (parse_f32(&v[..e])?, &v[e..])
        }
    };
    let px = match unit {
        "px" | "" => num,
        "em" => num * em,
        "rem" => num * 16.0,
        "pt" => num * 4.0 / 3.0,
        "pc" => num * 16.0,
        "in" => num * 96.0,
        "cm" => num * 37.8,
        "mm" => num * 3.78,
        "ch" => num * em * 0.55,
        "ex" => num * em * 0.5,
        "vw" => num * viewport.0 as f32 / 100.0,
        "vh" => num * viewport.1 as f32 / 100.0,
        "vmin" => num * viewport.0.min(viewport.1) as f32 / 100.0,
        "vmax" => num * viewport.0.max(viewport.1) as f32 / 100.0,
        "%" => return Some(Len::Val { px: 0.0, pct: num }),
        "fr" => return Some(Len::Auto),
        _ => return None,
    };
    Some(Len::px(px))
}

/// calc() with + - * / over lengths and numbers.
fn calc(expr: &str, em: f32, viewport: (i32, i32)) -> Option<Len> {
    // tokens separated by spaces around + and -
    let mut total_px = 0.0;
    let mut total_pct = 0.0;
    let mut sign = 1.0;
    for term in expr.split_whitespace() {
        match term {
            "+" => sign = 1.0,
            "-" => sign = -1.0,
            t => {
                // a product like 2*var or 100%/3
                let mut px = 0.0;
                let mut pct = 0.0;
                let mut factor = 1.0;
                let mut have_len = false;
                for (i, f) in t.split(['*', '/']).enumerate() {
                    let divide = i > 0 && t[..t.find(f).unwrap_or(0)].ends_with('/');
                    let f = f.trim_matches(|c| c == '(' || c == ')');
                    if let Some(n) = parse_f32(f) {
                        if divide {
                            if n != 0.0 {
                                factor /= n;
                            }
                        } else {
                            factor *= n;
                        }
                    } else if let Some(Len::Val { px: p, pct: q }) = parse_len(f, em, viewport) {
                        px = p;
                        pct = q;
                        have_len = true;
                    }
                }
                if !have_len {
                    px = factor;
                    factor = 1.0;
                }
                total_px += sign * px * factor;
                total_pct += sign * pct * factor;
                sign = 1.0;
            }
        }
    }
    Some(Len::Val {
        px: total_px,
        pct: total_pct,
    })
}

fn apply(s: &mut Style, parent: &Style, name: &str, v: &str, vp: (i32, i32)) {
    let lower = v.to_ascii_lowercase();
    let v_l = lower.as_str();
    if v_l == "inherit" {
        inherit_one(s, parent, name);
        return;
    }
    let em = s.font_size;
    let len = |v: &str| parse_len(v, em, vp);
    match name {
        "display" => {
            s.display = match v_l.split_whitespace().last().unwrap_or("") {
                "none" => Display::None,
                "block" | "flow-root" | "flow" | "run-in" => Display::Block,
                "inline" => Display::Inline,
                "inline-block" | "inline-table" => Display::InlineBlock,
                "flex" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox" => Display::Flex,
                "inline-flex" | "-webkit-inline-flex" => Display::InlineFlex,
                "grid" | "inline-grid" => Display::Grid,
                "list-item" => Display::ListItem,
                "table" => Display::Table,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-row-group" | "table-header-group" | "table-footer-group" => {
                    Display::TableRowGroup
                }
                "table-caption" => Display::TableCaption,
                "contents" => Display::Contents,
                _ => s.display,
            };
            if v_l.starts_with("inline") && v_l.ends_with("flex") {
                s.display = Display::InlineFlex;
            }
        }
        "color" => {
            if let Some(c) = parse_color_with(v, parent.color) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = parse_color_with(v, s.color) {
                s.background = c;
            }
        }
        "background" => {
            // the colour and image parts of the shorthand
            s.background = TRANSPARENT;
            for part in split_top(v, ' ') {
                if let Some(c) = parse_color_with(part, s.color) {
                    s.background = c;
                }
            }
            if let Some(pos) = v.find("url(") {
                s.background_image = css::parse_url(&v[pos..]);
            }
            if v_l.contains("gradient(") {
                // use the first colour of a gradient
                if let Some(c) = first_color(v, s.color) {
                    s.background = c;
                }
            }
        }
        "background-image" => {
            if let Some(pos) = v.find("url(") {
                s.background_image = css::parse_url(&v[pos..]);
            } else if v_l.contains("gradient(") {
                if let Some(c) = first_color(v, s.color) {
                    s.background = c;
                }
            }
        }
        "font-size" => {
            let size = match v_l {
                "xx-small" => Some(9.0),
                "x-small" => Some(10.0),
                "small" => Some(13.0),
                "medium" => Some(16.0),
                "large" => Some(18.0),
                "x-large" => Some(24.0),
                "xx-large" => Some(32.0),
                "xxx-large" => Some(48.0),
                "smaller" => Some(parent.font_size / 1.2),
                "larger" => Some(parent.font_size * 1.2),
                _ => parse_len(v, parent.font_size, vp).map(|l| match l {
                    Len::Val { px, pct } => px + pct * parent.font_size / 100.0,
                    Len::Auto => parent.font_size,
                }),
            };
            if let Some(sz) = size {
                s.font_size = sz.clamp(6.0, 96.0);
            }
        }
        "font-weight" => {
            s.bold = match v_l {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<i32>().map(|w| w >= 600).unwrap_or(s.bold),
            }
        }
        "font-style" => s.italic = v_l.starts_with("italic") || v_l.starts_with("oblique"),
        "font-family" => s.mono = is_mono_family(v_l),
        "font" => {
            // [style] [weight] size[/line-height] family
            s.bold = false;
            s.italic = false;
            for (i, part) in split_top(v, ' ').iter().enumerate() {
                let p = part.to_ascii_lowercase();
                if p == "bold" || p.parse::<i32>().is_ok_and(|w| w >= 600) {
                    s.bold = true;
                } else if p == "italic" || p == "oblique" {
                    s.italic = true;
                } else if p
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '.')
                {
                    let (size, lh) = match p.split_once('/') {
                        Some((a, b)) => (a, Some(b)),
                        None => (p.as_str(), None),
                    };
                    if let Some(Len::Val { px, pct }) = parse_len(size, parent.font_size, vp) {
                        s.font_size = (px + pct * parent.font_size / 100.0).clamp(6.0, 96.0);
                    }
                    if let Some(lh) = lh {
                        apply(s, parent, "line-height", lh, vp);
                    }
                    let rest: Vec<&str> = split_top(v, ' ').into_iter().skip(i + 1).collect();
                    s.mono = is_mono_family(&rest.join(" ").to_ascii_lowercase());
                    break;
                }
            }
        }
        "line-height" => {
            s.line_height = match v_l {
                "normal" => None,
                _ => match parse_f32(v_l) {
                    Some(n) => Some(n.min(4.0)),
                    None => match parse_len(v, s.font_size, vp) {
                        Some(Len::Val { px, pct }) => {
                            Some((px + pct * s.font_size / 100.0).max(4.01))
                        }
                        _ => None,
                    },
                },
            }
        }
        "text-decoration" | "text-decoration-line" => {
            s.underline = v_l.contains("underline");
            s.line_through = v_l.contains("line-through");
        }
        "text-align" => {
            s.text_align = match v_l {
                "center" | "-webkit-center" | "-moz-center" => Align::Center,
                "right" | "end" | "-webkit-right" => Align::Right,
                "justify" => Align::Justify,
                "left" | "start" | "-webkit-left" => Align::Left,
                _ => s.text_align,
            }
        }
        "text-transform" => {
            s.transform = match v_l {
                "uppercase" => Transform::Upper,
                "lowercase" => Transform::Lower,
                "capitalize" => Transform::Capitalize,
                _ => Transform::None,
            }
        }
        "white-space" => {
            s.pre = matches!(v_l, "pre" | "pre-wrap" | "break-spaces" | "pre-line");
            s.nowrap = matches!(v_l, "nowrap" | "pre");
        }
        "margin" => {
            if let Some(b) = box4(v, &len) {
                s.margin = b;
            }
        }
        "padding" => {
            if let Some(b) = box4(v, &len) {
                s.padding = b;
            }
        }
        "margin-top"
        | "margin-right"
        | "margin-bottom"
        | "margin-left"
        | "margin-block-start"
        | "margin-block-end"
        | "margin-inline-start"
        | "margin-inline-end" => {
            if let Some(l) = len(v) {
                for &i in side(name) {
                    s.margin[i] = l;
                }
            }
        }
        "margin-block" | "margin-inline" | "padding-block" | "padding-inline" => {
            let parts: Vec<Len> = split_top(v, ' ').iter().filter_map(|p| len(p)).collect();
            if let Some(&a) = parts.first() {
                let b = parts.get(1).copied().unwrap_or(a);
                let idx = if name.ends_with("block") {
                    [0, 2]
                } else {
                    [3, 1]
                };
                let target = if name.starts_with("margin") {
                    &mut s.margin
                } else {
                    &mut s.padding
                };
                target[idx[0]] = a;
                target[idx[1]] = b;
            }
        }
        "padding-top"
        | "padding-right"
        | "padding-bottom"
        | "padding-left"
        | "padding-block-start"
        | "padding-block-end"
        | "padding-inline-start"
        | "padding-inline-end" => {
            if let Some(l) = len(v) {
                for &i in side(name) {
                    s.padding[i] = l;
                }
            }
        }
        "border"
        | "border-top"
        | "border-right"
        | "border-bottom"
        | "border-left"
        | "border-block"
        | "border-inline"
        | "border-block-start"
        | "border-block-end"
        | "border-inline-start"
        | "border-inline-end" => {
            let (w, c) = parse_border(v, s.color, em, vp);
            let sides: Vec<usize> = match name {
                "border" => alloc::vec![0, 1, 2, 3],
                "border-block" => alloc::vec![0, 2],
                "border-inline" => alloc::vec![1, 3],
                n => side(n).to_vec(),
            };
            for i in sides {
                s.border[i] = w;
                s.border_color[i] = c;
            }
        }
        "border-width" => {
            if let Some(b) = box4(v, &|p: &str| border_width(p, em, vp).map(Len::px)) {
                for (dst, l) in s.border.iter_mut().zip(b) {
                    *dst = l.or0(0) as f32;
                }
            }
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            if let Some(w) = border_width(v, em, vp) {
                for &i in side(name) {
                    s.border[i] = w;
                }
            }
        }
        "border-color" => {
            let parts = split_top(v, ' ');
            let colors: Vec<Color> = parts
                .iter()
                .filter_map(|p| parse_color_with(p, s.color))
                .collect();
            if !colors.is_empty() {
                let b = expand4(&colors);
                s.border_color = b;
            }
        }
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            if let Some(c) = parse_color_with(v, s.color) {
                for &i in side(name) {
                    s.border_color[i] = c;
                }
            }
        }
        "border-style" => {
            if v_l == "none" || v_l == "hidden" {
                s.border = [0.0; 4];
            }
        }
        "border-radius" => {
            if let Some(Len::Val { px, pct }) = len(split_top(v, ' ')[0]) {
                s.radius = if pct > 0.0 { 999.0 } else { px };
            }
        }
        "width" => s.width = len(v).unwrap_or(s.width),
        "height" => s.height = len(v).unwrap_or(s.height),
        "min-width" => s.min_width = len(v).unwrap_or(s.min_width),
        "max-width" => s.max_width = len(v).unwrap_or(s.max_width),
        "min-height" => s.min_height = len(v).unwrap_or(s.min_height),
        "max-height" => s.max_height = len(v).unwrap_or(s.max_height),
        "inline-size" => s.width = len(v).unwrap_or(s.width),
        "max-inline-size" => s.max_width = len(v).unwrap_or(s.max_width),
        "box-sizing" => s.border_box = v_l == "border-box",
        "list-style" | "list-style-type" => {
            for part in v_l.split_whitespace() {
                s.list_style = match part {
                    "none" => ListStyle::None,
                    "disc" => ListStyle::Disc,
                    "circle" => ListStyle::Circle,
                    "square" => ListStyle::Square,
                    "decimal" | "decimal-leading-zero" => ListStyle::Decimal,
                    "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
                    "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
                    "lower-roman" => ListStyle::LowerRoman,
                    "upper-roman" => ListStyle::UpperRoman,
                    _ => continue,
                };
            }
        }
        "visibility" => s.visible = v_l == "visible",
        "opacity" => s.opacity = parse_f32(v_l).unwrap_or(1.0),
        "flex-direction" => s.flex_column = v_l.starts_with("column"),
        "flex-wrap" => s.flex_wrap = v_l == "wrap" || v_l == "wrap-reverse",
        "flex-flow" => {
            s.flex_column = v_l.contains("column");
            s.flex_wrap = v_l.split_whitespace().any(|p| p == "wrap");
        }
        "justify-content" => {
            s.justify = match v_l.split_whitespace().last().unwrap_or("") {
                "center" => Justify::Center,
                "flex-end" | "end" | "right" => Justify::End,
                "space-between" => Justify::SpaceBetween,
                "space-around" => Justify::SpaceAround,
                "space-evenly" => Justify::SpaceEvenly,
                _ => Justify::Start,
            }
        }
        "align-items" | "place-items" => {
            let first = v_l.split_whitespace().next().unwrap_or("");
            s.align_center = first == "center";
            s.align_end = matches!(first, "flex-end" | "end" | "self-end");
            s.align_stretch = matches!(first, "stretch" | "normal");
        }
        "top" | "right" | "bottom" | "left" | "inset" => {
            if name == "inset" {
                if let Some(b) = box4(v, &len) {
                    s.inset = b;
                }
            } else if let Some(l) = len(v) {
                let i = match name {
                    "top" => 0,
                    "right" => 1,
                    "bottom" => 2,
                    _ => 3,
                };
                s.inset[i] = l;
            }
        }
        "border-spacing" => {
            if let Some(Len::Val { px, .. }) = len(split_top(v, ' ')[0]) {
                s.border_spacing = px;
            }
        }
        "border-collapse" => {
            if v_l == "collapse" {
                s.border_spacing = 0.0;
            }
        }
        "gap" | "grid-gap" | "column-gap" | "grid-column-gap" => {
            if let Some(Len::Val { px, .. }) = len(split_top(v, ' ').last().copied().unwrap_or("0"))
            {
                s.gap = px;
            }
        }
        "flex" => {
            let parts: Vec<&str> = v_l.split_whitespace().collect();
            match parts.as_slice() {
                ["none"] => {
                    s.flex_grow = 0.0;
                    s.flex_shrink = 0.0;
                }
                ["auto"] => s.flex_grow = 1.0,
                [g, rest @ ..] => {
                    if let Some(g) = parse_f32(g) {
                        s.flex_grow = g;
                        s.flex_basis = Len::ZERO;
                        if let Some(b) = rest.last().and_then(|b| len(b)) {
                            if parse_f32(rest.last().unwrap()).is_none() {
                                s.flex_basis = b;
                            }
                        }
                    } else if let Some(b) = len(g) {
                        s.flex_basis = b;
                        s.flex_grow = 1.0;
                    }
                }
                _ => {}
            }
        }
        "flex-grow" => s.flex_grow = parse_f32(v_l).unwrap_or(0.0),
        "flex-shrink" => s.flex_shrink = parse_f32(v_l).unwrap_or(1.0),
        "flex-basis" => s.flex_basis = len(v).unwrap_or(Len::Auto),
        "order" => s.order = v_l.parse().unwrap_or(0),
        "grid-template-columns" => {
            s.grid_columns = count_grid_columns(v_l);
        }
        "position" => {
            s.position = match v_l {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        "float" => {
            s.float_left = v_l == "left" || v_l == "inline-start";
            s.float_right = v_l == "right" || v_l == "inline-end";
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            if v_l != "visible" {
                s.clip = true;
            }
        }
        "clip" => {
            if v_l.starts_with("rect(") {
                s.clip = true;
                s.width = Len::px(1.0);
                s.height = Len::px(1.0);
            }
        }
        "clip-path" => {
            if v_l.contains("inset(50%") || v_l.contains("inset(100%") {
                s.visible = false;
            }
        }
        "vertical-align" => s.vertical_middle = v_l == "middle",
        "content-visibility" if v_l == "hidden" => s.display = Display::None,
        _ => {}
    }
}

fn count_grid_columns(v: &str) -> usize {
    if let Some(rest) = v.strip_prefix("repeat(") {
        let n = rest.split(',').next().unwrap_or("").trim();
        return n.parse().unwrap_or(3).min(12);
    }
    split_top(v, ' ')
        .iter()
        .filter(|p| !p.trim().is_empty())
        .count()
        .min(12)
}

fn inherit_one(s: &mut Style, p: &Style, name: &str) {
    match name {
        "color" => s.color = p.color,
        "background-color" | "background" => s.background = p.background,
        "font-size" => s.font_size = p.font_size,
        "font-weight" => s.bold = p.bold,
        "display" => s.display = p.display,
        "width" => s.width = p.width,
        "height" => s.height = p.height,
        _ => {}
    }
}

fn is_mono_family(v: &str) -> bool {
    let first = v
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c| c == '"' || c == '\'');
    first.contains("mono")
        || first.contains("courier")
        || first.contains("consolas")
        || first.contains("menlo")
        || first.contains("code")
        || v.trim() == "monospace"
}

fn side(name: &str) -> &'static [usize] {
    if name.contains("top") || name.contains("block-start") {
        &[0]
    } else if name.contains("right") || name.contains("inline-end") {
        &[1]
    } else if name.contains("bottom") || name.contains("block-end") {
        &[2]
    } else if name.contains("left") || name.contains("inline-start") {
        &[3]
    } else {
        &[]
    }
}

fn expand4<T: Copy>(v: &[T]) -> [T; 4] {
    match v.len() {
        1 => [v[0]; 4],
        2 => [v[0], v[1], v[0], v[1]],
        3 => [v[0], v[1], v[2], v[1]],
        _ => [v[0], v[1], v[2], v[3]],
    }
}

fn box4(v: &str, len: &dyn Fn(&str) -> Option<Len>) -> Option<[Len; 4]> {
    let parts: Vec<Len> = split_top(v, ' ')
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| len(p))
        .collect::<Option<Vec<Len>>>()?;
    (!parts.is_empty()).then(|| expand4(&parts))
}

fn border_width(v: &str, em: f32, vp: (i32, i32)) -> Option<f32> {
    match v.trim() {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        v => match parse_len(v, em, vp)? {
            Len::Val { px, .. } => Some(px),
            Len::Auto => None,
        },
    }
}

fn parse_border(v: &str, current: Color, em: f32, vp: (i32, i32)) -> (f32, Color) {
    let mut width = 3.0;
    let mut color = current;
    let mut none = false;
    for part in split_top(v, ' ') {
        let p = part.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        if p == "none" || p == "hidden" || p == "0" {
            none = true;
        } else if matches!(
            p.as_str(),
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset"
        ) {
        } else if let Some(w) = border_width(&p, em, vp) {
            width = w;
        } else if let Some(c) = parse_color_with(part, current) {
            color = c;
        }
    }
    (if none { 0.0 } else { width }, color)
}

/// Things that depend on several properties at once.
fn fixups(s: &mut Style, parent: &Style, tag: &str) {
    // screen-reader-only and off-screen elements
    if matches!(s.position, Position::Absolute | Position::Fixed)
        && (matches!(s.width, Len::Val { px, pct } if px <= 1.0 && pct == 0.0)
            || matches!(s.height, Len::Val { px, pct } if px <= 1.0 && pct == 0.0))
    {
        s.display = Display::None;
    }
    if s.position == Position::Fixed {
        // banners, pop-ups and toolbars: a static page has nowhere to pin them
        s.display = Display::None;
    }
    if parent.display == Display::Flex
        || parent.display == Display::InlineFlex
        || parent.display == Display::Grid
    {
        // flex items are blockified
        if s.display == Display::Inline {
            s.display = Display::Block;
        }
    }
    if (s.float_left || s.float_right) && s.display == Display::Inline {
        s.display = Display::InlineBlock;
    }
    if tag == "br" {
        s.display = Display::Inline;
    }
}

// ---- colours --------------------------------------------------------------------

pub fn parse_color(v: &str) -> Option<Color> {
    parse_color_with(v, BLACK)
}

fn first_color(v: &str, current: Color) -> Option<Color> {
    let inner = v.split_once('(')?.1;
    split_top(inner, ',')
        .iter()
        .filter_map(|p| {
            let p = p.trim();
            let p = p.split_whitespace().next().unwrap_or(p);
            if p.contains('(') {
                // rgb(...) inside the gradient: take everything to its ')'
                let start = inner.find(p)?;
                let end = inner[start..].find(')')? + start + 1;
                parse_color_with(&inner[start..end], current)
            } else {
                parse_color_with(p, current)
            }
        })
        .next()
}

pub fn parse_color_with(v: &str, current: Color) -> Option<Color> {
    let v = v.trim();
    let l = v.to_ascii_lowercase();
    if let Some(hex) = l.strip_prefix('#') {
        let h = |i: usize, n: usize| u32::from_str_radix(hex.get(i..i + n)?, 16).ok();
        return match hex.len() {
            3 | 4 => {
                let r = h(0, 1)? * 17;
                let g = h(1, 1)? * 17;
                let b = h(2, 1)? * 17;
                let a = if hex.len() == 4 { h(3, 1)? * 17 } else { 255 };
                Some(a << 24 | r << 16 | g << 8 | b)
            }
            6 | 8 => {
                let rgb = h(0, 6)?;
                let a = if hex.len() == 8 { h(6, 2)? } else { 255 };
                Some(a << 24 | rgb)
            }
            _ => None,
        };
    }
    if let Some(args) = l
        .strip_prefix("rgba(")
        .or_else(|| l.strip_prefix("rgb("))
        .and_then(|a| a.strip_suffix(')'))
    {
        let nums: Vec<&str> = args
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .collect();
        if nums.len() < 3 {
            return None;
        }
        let chan = |s: &str| -> Option<u32> {
            Some(
                if let Some(p) = s.strip_suffix('%') {
                    (parse_f32(p)? * 2.55) as u32
                } else {
                    parse_f32(s)? as u32
                }
                .min(255),
            )
        };
        let a = match nums.get(3) {
            Some(s) => {
                if let Some(p) = s.strip_suffix('%') {
                    (parse_f32(p)? * 2.55) as u32
                } else {
                    (parse_f32(s)? * 255.0) as u32
                }
            }
            None => 255,
        }
        .min(255);
        return Some(a << 24 | chan(nums[0])? << 16 | chan(nums[1])? << 8 | chan(nums[2])?);
    }
    if let Some(args) = l
        .strip_prefix("hsla(")
        .or_else(|| l.strip_prefix("hsl("))
        .and_then(|a| a.strip_suffix(')'))
    {
        let nums: Vec<&str> = args
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .collect();
        if nums.len() < 3 {
            return None;
        }
        let h = parse_f32(nums[0].trim_end_matches("deg"))? / 360.0;
        let s = parse_f32(nums[1].trim_end_matches('%'))? / 100.0;
        let lt = parse_f32(nums[2].trim_end_matches('%'))? / 100.0;
        let a = nums
            .get(3)
            .and_then(|a| {
                if let Some(p) = a.strip_suffix('%') {
                    parse_f32(p).map(|p| p / 100.0)
                } else {
                    parse_f32(a)
                }
            })
            .unwrap_or(1.0);
        let (r, g, b) = hsl_to_rgb(h - (h as i32) as f32, s, lt);
        return Some(((a * 255.0) as u32).min(255) << 24 | r << 16 | g << 8 | b);
    }
    Some(
        match l.as_str() {
            "transparent" => TRANSPARENT,
            "currentcolor" => current,
            "black" => 0x000000,
            "white" => 0xffffff,
            "red" => 0xff0000,
            "green" => 0x008000,
            "blue" => 0x0000ff,
            "yellow" => 0xffff00,
            "orange" => 0xffa500,
            "purple" => 0x800080,
            "gray" | "grey" => 0x808080,
            "silver" => 0xc0c0c0,
            "maroon" => 0x800000,
            "navy" => 0x000080,
            "teal" => 0x008080,
            "olive" => 0x808000,
            "lime" => 0x00ff00,
            "aqua" | "cyan" => 0x00ffff,
            "fuchsia" | "magenta" => 0xff00ff,
            "lightgray" | "lightgrey" => 0xd3d3d3,
            "darkgray" | "darkgrey" => 0xa9a9a9,
            "dimgray" | "dimgrey" => 0x696969,
            "gainsboro" => 0xdcdcdc,
            "whitesmoke" => 0xf5f5f5,
            "darkblue" => 0x00008b,
            "darkred" => 0x8b0000,
            "darkgreen" => 0x006400,
            "lightblue" => 0xadd8e6,
            "lightgreen" => 0x90ee90,
            "lightyellow" => 0xffffe0,
            "skyblue" => 0x87ceeb,
            "steelblue" => 0x4682b4,
            "royalblue" => 0x4169e1,
            "dodgerblue" => 0x1e90ff,
            "cornflowerblue" => 0x6495ed,
            "slategray" | "slategrey" => 0x708090,
            "lightslategray" => 0x778899,
            "gold" => 0xffd700,
            "pink" => 0xffc0cb,
            "brown" => 0xa52a2a,
            "crimson" => 0xdc143c,
            "tomato" => 0xff6347,
            "coral" => 0xff7f50,
            "salmon" => 0xfa8072,
            "beige" => 0xf5f5dc,
            "ivory" => 0xfffff0,
            "linen" => 0xfaf0e6,
            "wheat" => 0xf5deb3,
            "tan" => 0xd2b48c,
            "khaki" => 0xf0e68c,
            "indigo" => 0x4b0082,
            "violet" => 0xee82ee,
            "orchid" => 0xda70d6,
            "plum" => 0xdda0dd,
            "lavender" => 0xe6e6fa,
            "aliceblue" => 0xf0f8ff,
            "azure" => 0xf0ffff,
            "honeydew" => 0xf0fff0,
            "mintcream" => 0xf5fffa,
            "seashell" => 0xfff5ee,
            "snow" => 0xfffafa,
            "ghostwhite" => 0xf8f8ff,
            "floralwhite" => 0xfffaf0,
            "oldlace" => 0xfdf5e6,
            "cornsilk" => 0xfff8dc,
            "lemonchiffon" => 0xfffacd,
            "papayawhip" => 0xffefd5,
            "blanchedalmond" => 0xffebcd,
            "bisque" => 0xffe4c4,
            "moccasin" => 0xffe4b5,
            "peachpuff" => 0xffdab9,
            "mistyrose" => 0xffe4e1,
            "lavenderblush" => 0xfff0f5,
            "antiquewhite" => 0xfaebd7,
            "firebrick" => 0xb22222,
            "darkorange" => 0xff8c00,
            "orangered" => 0xff4500,
            "seagreen" => 0x2e8b57,
            "forestgreen" => 0x228b22,
            "limegreen" => 0x32cd32,
            "darkslategray" | "darkslategrey" => 0x2f4f4f,
            "midnightblue" => 0x191970,
            "darkcyan" => 0x008b8b,
            "cadetblue" => 0x5f9ea0,
            "turquoise" => 0x40e0d0,
            "chocolate" => 0xd2691e,
            "sienna" => 0xa0522d,
            "goldenrod" => 0xdaa520,
            "darkviolet" => 0x9400d3,
            "rebeccapurple" => 0x663399,
            "slateblue" => 0x6a5acd,
            "lightcoral" => 0xf08080,
            "lightpink" => 0xffb6c1,
            "hotpink" => 0xff69b4,
            "deeppink" => 0xff1493,
            "lightcyan" => 0xe0ffff,
            "lightsteelblue" => 0xb0c4de,
            "powderblue" => 0xb0e0e6,
            "paleturquoise" => 0xafeeee,
            "palegreen" => 0x98fb98,
            "darkkhaki" => 0xbdb76b,
            "canvas" | "field" | "buttonface" => 0xffffff,
            "canvastext" | "fieldtext" | "buttontext" => 0x000000,
            "linktext" => 0x1a0dab,
            "graytext" => 0x808080,
            _ => return None,
        } | if l == "transparent" || l == "currentcolor" {
            0
        } else {
            0xff00_0000
        },
    )
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u32, u32, u32) {
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        ((v * 255.0) as u32).min(255)
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// The text of a node with the style's text-transform applied.
pub fn transform_text(text: &str, t: Transform) -> String {
    match t {
        Transform::None => text.to_string(),
        Transform::Upper => text.to_uppercase(),
        Transform::Lower => text.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::new();
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}
