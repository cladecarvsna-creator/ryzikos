//! Calculator: + - × ÷ with the mouse or the keyboard.

use core::fmt::Write;

use super::canvas::{Canvas, Rect};
use super::text;
use super::theme;
use super::{MouseEvent, MouseKind};
use crate::keyboard::Key;
use crate::StackString;

const BUTTON_W: i32 = 76;
const BUTTON_H: i32 = 52;
const GAP: i32 = 4;
const MARGIN: i32 = 8;
const DISPLAY_H: i32 = 84;
const KEYS_Y: i32 = MARGIN + DISPLAY_H + 10;
pub const CLIENT_W: i32 = 2 * MARGIN + 4 * BUTTON_W + 3 * GAP;
pub const CLIENT_H: i32 = KEYS_Y + 5 * BUTTON_H + 4 * GAP + MARGIN;

/// Most characters an entry can have.
const MAX_ENTRY: usize = 16;

const LABELS: [&str; 20] = [
    "C", "DEL", "%", "÷", //
    "7", "8", "9", "×", //
    "4", "5", "6", "-", //
    "1", "2", "3", "+", //
    "±", "0", ".", "=",
];

fn button_rect(i: usize) -> Rect {
    let (col, row) = ((i % 4) as i32, (i / 4) as i32);
    Rect::new(
        MARGIN + col * (BUTTON_W + GAP),
        KEYS_Y + row * (BUTTON_H + GAP),
        BUTTON_W,
        BUTTON_H,
    )
}

pub struct Calc {
    entry: StackString<24>,
    /// The left operand and the operator waiting for the right one.
    acc: f64,
    op: Option<char>,
    /// The next digit starts a new entry (after an operator or `=`).
    fresh: bool,
    error: bool,
    /// Button held down with the mouse, drawn pressed.
    pressed: Option<usize>,
}

impl Calc {
    pub fn new() -> Self {
        let mut calc = Self {
            entry: StackString::new(),
            acc: 0.0,
            op: None,
            fresh: true,
            error: false,
            pressed: None,
        };
        calc.clear();
        calc
    }

    fn clear(&mut self) {
        self.entry.clear();
        self.entry.push_str("0");
        self.acc = 0.0;
        self.op = None;
        self.fresh = true;
        self.error = false;
    }

    fn value(&self) -> f64 {
        parse(self.entry.as_str())
    }

    fn set_value(&mut self, v: f64) {
        self.entry.clear();
        if !v.is_finite() {
            self.error = true;
            self.entry.push_str("Error");
            return;
        }
        format_number(&mut self.entry, v);
    }

    /// Apply the waiting operator to the accumulator and the entry.
    fn apply(&mut self) {
        let rhs = self.value();
        let result = match self.op {
            Some('+') => self.acc + rhs,
            Some('-') => self.acc - rhs,
            Some('×') => self.acc * rhs,
            Some('÷') => {
                if rhs == 0.0 {
                    f64::NAN
                } else {
                    self.acc / rhs
                }
            }
            _ => rhs,
        };
        self.set_value(result);
        self.acc = result;
    }

    /// Handle one button, by its label.
    fn press(&mut self, label: &str) {
        if self.error && label != "C" {
            self.clear();
        }
        match label {
            "C" => self.clear(),
            "DEL" => {
                if !self.fresh {
                    self.entry.pop();
                    if matches!(self.entry.as_str(), "" | "-") {
                        self.entry.clear();
                        self.entry.push_str("0");
                    }
                }
            }
            "±" => {
                if self.entry.as_str() != "0" {
                    let v = -self.value();
                    self.set_value(v);
                }
            }
            "%" => {
                let v = self.value() / 100.0;
                let v = if self.op.is_some() { self.acc * v } else { v };
                self.set_value(v);
                self.fresh = true;
            }
            "." => {
                if self.fresh {
                    self.entry.clear();
                    self.entry.push_str("0");
                    self.fresh = false;
                }
                if !self.entry.as_str().contains('.') && self.entry.len() < MAX_ENTRY {
                    self.entry.push_str(".");
                }
            }
            "=" => {
                if self.op.is_some() {
                    self.apply();
                    self.op = None;
                }
                self.fresh = true;
            }
            "+" | "-" | "×" | "÷" => {
                if self.op.is_some() && !self.fresh {
                    self.apply();
                } else {
                    self.acc = self.value();
                }
                self.op = label.chars().next();
                self.fresh = true;
            }
            digit => {
                if self.fresh || self.entry.as_str() == "0" {
                    self.entry.clear();
                    self.fresh = false;
                }
                let digits = self
                    .entry
                    .as_str()
                    .chars()
                    .filter(char::is_ascii_digit)
                    .count();
                if digits < MAX_ENTRY - 1 {
                    self.entry.push_str(digit);
                }
            }
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        let label = match key {
            Key::Char(c) => match c {
                '0'..='9' => {
                    let mut buf = [0u8; 4];
                    let s: &str = c.encode_utf8(&mut buf);
                    self.press(s);
                    return true;
                }
                '.' | ',' | 'ю' | 'б' => ".",
                '+' => "+",
                '-' => "-",
                '*' | 'x' | 'X' | 'ч' => "×",
                '/' => "÷",
                '%' => "%",
                '=' => "=",
                'c' | 'C' | 'с' | 'С' => "C",
                _ => return false,
            },
            Key::Enter => "=",
            Key::Backspace => "DEL",
            Key::Escape => "C",
            _ => return false,
        };
        self.press(label);
        true
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } => {
                self.pressed = (0..LABELS.len()).find(|&i| button_rect(i).contains(ev.x, ev.y));
                self.pressed.is_some()
            }
            MouseKind::Up => match self.pressed.take() {
                Some(i) => {
                    // like a real button, releasing outside cancels
                    if button_rect(i).contains(ev.x, ev.y) {
                        self.press(LABELS[i]);
                    }
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::face());

        // Windows 11 shows the number straight on the window, no box
        let display = Rect::new(MARGIN, MARGIN, CLIENT_W - 2 * MARGIN, DISPLAY_H);
        let dark = theme::text();
        let mut pending = StackString::<32>::new();
        if let Some(op) = self.op {
            let mut acc = StackString::<24>::new();
            format_number(&mut acc, self.acc);
            let _ = write!(pending, "{} {}", acc.as_str(), op);
        }
        let small = theme::text_dim();
        let w = super::canvas::text_width(pending.as_str());
        c.draw_text(
            display.right() - 6 - w,
            display.y + 4,
            pending.as_str(),
            small,
        );
        // the entry, in double-size digits
        draw_big_text(
            c,
            display.right() - 6,
            display.bottom(),
            self.entry.as_str(),
            dark,
        );

        for (i, label) in LABELS.iter().enumerate() {
            let r = button_rect(i);
            let pressed = self.pressed == Some(i);
            match *label {
                "=" => theme::accent_button(c, r, label, pressed),
                "C" | "DEL" | "+" | "-" | "×" | "÷" | "%" => {
                    theme::colored_button(c, r, label, theme::control(), pressed)
                }
                _ => theme::colored_button(c, r, label, theme::control_lit(), pressed),
            }
        }
    }
}

/// The entry in the largest font that fits, right-aligned to `right`
/// with its line bottom at `bottom`.
fn draw_big_text(c: &mut Canvas, right: i32, bottom: i32, s: &str, color: u32) {
    let room = CLIENT_W - 2 * MARGIN - 12;
    let font = [&text::LARGE, &text::TITLE, &text::UI]
        .into_iter()
        .find(|f| f.width(s) <= room)
        .unwrap_or(&text::UI);
    c.draw_text_in(
        font,
        right - font.width(s),
        bottom - font.line_height,
        s,
        color,
    );
}

fn parse(s: &str) -> f64 {
    let (negative, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mut value = 0.0;
    let mut scale = 1.0;
    let mut after_point = false;
    for ch in s.chars() {
        match ch {
            '0'..='9' => {
                let d = (ch as u8 - b'0') as f64;
                if after_point {
                    scale /= 10.0;
                    value += d * scale;
                } else {
                    value = value * 10.0 + d;
                }
            }
            '.' => after_point = true,
            _ => {}
        }
    }
    if negative {
        -value
    } else {
        value
    }
}

/// Write a number the way a calculator shows it: no trailing zeros, at
/// most `MAX_ENTRY` characters.
fn format_number<const N: usize>(out: &mut StackString<N>, v: f64) {
    let v = if v == 0.0 { 0.0 } else { v }; // no "-0"
    if v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-9) {
        let _ = write!(out, "{:.6e}", v);
        return;
    }
    let integer = v as i64;
    if integer as f64 == v {
        let _ = write!(out, "{}", integer);
        return;
    }
    // as many decimals as fit
    let int_digits = {
        let mut n = 1;
        let mut i = integer.unsigned_abs();
        while i >= 10 {
            i /= 10;
            n += 1;
        }
        n
    };
    let decimals = (MAX_ENTRY - 2).saturating_sub(int_digits).clamp(1, 10);
    let _ = write!(out, "{:.*}", decimals, v);
    while out.as_str().ends_with('0') {
        out.pop();
    }
    if out.as_str().ends_with('.') {
        out.pop();
    }
}
