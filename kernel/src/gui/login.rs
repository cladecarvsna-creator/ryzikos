//! The sign-in screens, in the style of Windows 11. First the lock
//! screen: the wallpaper with a big clock and the date. A key or a click
//! slides it up and shows the sign-in panel over a blurred, darker copy
//! of the wallpaper: the user's picture and name, a password box, the
//! list of users in the corner and a power button.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use super::anim::{self, Fader, Tween, ONE};
use super::canvas::{fast_mix, mix, rgb, Canvas, Color, Dirty, Rect};
use super::start::{power_symbol, restart_symbol};
use super::text::{CLOCK, HEADING, UI};
use super::theme;
use crate::keyboard::Key;
use crate::{interrupts, rtc, serial, users, StackString};

pub enum Outcome {
    None,
    SignedIn,
    Restart,
    ShutDown,
}

/// Something on the panel that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    User(usize),
    Submit,
    Power,
    Restart,
    ShutDown,
}

const BIG_AVATAR: i32 = 176;
const SMALL_AVATAR: i32 = 36;
const FIELD_W: i32 = 296;
const FIELD_H: i32 = 36;
const SHAKE_TICKS: u64 = interrupts::TIMER_HZ * 2 / 5;
const BLINK_TICKS: u64 = interrupts::TIMER_HZ / 2;
const WHITE: Color = 0xffffff;

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub struct Login {
    width: i32,
    height: i32,
    /// The wallpaper blurred and darkened, behind the panel.
    backdrop: &'static mut [u32],
    big_avatar: Vec<u32>,
    small_avatar: Vec<u32>,
    /// How far the lock screen has slid away: 0 shows it, ONE shows
    /// the panel.
    reveal: Tween,
    /// Whether the lock screen moved in the last frame.
    sliding: bool,
    selected: usize,
    password: StackString<64>,
    wrong: bool,
    /// When the password box started shaking after a wrong password.
    shake: Option<u64>,
    hover: Fader<Target>,
    power_open: bool,
    caret_on: bool,
    next_blink: u64,
    clock: StackString<8>,
    date: StackString<48>,
    /// Keyboard layout, shown next to the power button.
    pub layout: &'static str,
    pub dirty: Dirty,
}

impl Login {
    /// The blurred, darkened wallpaper, also behind Task View.
    pub fn backdrop(&self) -> &[u32] {
        self.backdrop
    }

    pub fn new(wallpaper: &[u32], backdrop: &'static mut [u32], width: i32, height: i32) -> Self {
        make_backdrop(wallpaper, backdrop, width as usize, height as usize);
        let mut login = Self {
            width,
            height,
            backdrop,
            big_avatar: avatar(BIG_AVATAR),
            small_avatar: avatar(SMALL_AVATAR),
            reveal: Tween::new(0, 0, 1),
            sliding: false,
            selected: 0,
            password: StackString::new(),
            wrong: false,
            shake: None,
            hover: Fader::new(anim::ms(120)),
            power_open: false,
            caret_on: true,
            next_blink: 0,
            clock: StackString::new(),
            date: StackString::new(),
            layout: "EN",
            dirty: Dirty::default(),
        };
        login.update_clock();
        login
    }

    /// The wallpaper changed: blur the new one.
    pub fn set_wallpaper(&mut self, wallpaper: &[u32]) {
        make_backdrop(
            wallpaper,
            self.backdrop,
            self.width as usize,
            self.height as usize,
        );
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Show the lock screen again, for the signed-in user or the last one.
    pub fn lock(&mut self) {
        self.reveal = Tween::new(0, 0, 1);
        self.selected = users::current().unwrap_or(self.selected);
        self.password.clear();
        self.wrong = false;
        self.shake = None;
        self.power_open = false;
        self.hover.jump(None);
        self.update_clock();
        self.dirty.add(self.screen());
        serial::write_str("\nlogin: lock screen\n");
    }

    fn panel_shown(&self) -> bool {
        self.reveal.target() == ONE
    }

    fn show_panel(&mut self) {
        if !self.panel_shown() {
            self.reveal.retarget(ONE, anim::ms(420));
            self.caret_on = true;
            self.next_blink = interrupts::ticks() + BLINK_TICKS;
            serial::write_str("login: password prompt\n");
        }
    }

    fn show_lock(&mut self) {
        if self.panel_shown() {
            self.reveal.retarget(0, anim::ms(420));
            self.power_open = false;
            self.hover.jump(None);
        }
    }

    /// Read the clock; mark the lock screen text if it changed.
    pub fn update_clock(&mut self) {
        let (h, m, _) = rtc::time();
        let (year, month, day) = rtc::date();
        let mut clock = StackString::<8>::new();
        let _ = write!(clock, "{:02}:{:02}", h, m);
        if clock.as_str() != self.clock.as_str() {
            self.clock = clock;
            self.date.clear();
            let weekday = rtc::weekday(year, month, day) as usize;
            let _ = write!(
                self.date,
                "{}, {} {}",
                DAYS[weekday % 7],
                MONTHS[month as usize - 1],
                day
            );
            self.dirty.add(self.clock_rect().union(&self.date_rect()));
        }
    }

    // ---- layout ------------------------------------------------------------

    fn clock_rect(&self) -> Rect {
        Rect::new(0, self.height * 14 / 100, self.width, CLOCK.line_height)
    }

    fn date_rect(&self) -> Rect {
        let clock = self.clock_rect();
        Rect::new(0, clock.bottom(), self.width, HEADING.line_height + 8)
    }

    fn top(&self) -> i32 {
        self.height / 2 - 240
    }

    fn avatar_rect(&self) -> Rect {
        Rect::new(
            (self.width - BIG_AVATAR) / 2,
            self.top(),
            BIG_AVATAR,
            BIG_AVATAR,
        )
    }

    fn name_rect(&self) -> Rect {
        Rect::new(0, self.top() + BIG_AVATAR + 20, self.width, 40)
    }

    fn field_rect(&self) -> Rect {
        Rect::new(
            (self.width - FIELD_W) / 2 + self.shake_offset(),
            self.top() + BIG_AVATAR + 84,
            FIELD_W,
            FIELD_H,
        )
    }

    /// The password box and room for it to shake.
    fn field_area(&self) -> Rect {
        let f = self.field_rect();
        Rect::new((self.width - FIELD_W) / 2 - 24, f.y, FIELD_W + 48, f.h)
    }

    fn submit_rect(&self) -> Rect {
        let f = self.field_rect();
        Rect::new(f.right() - 34, f.y + 3, 30, f.h - 6)
    }

    fn message_rect(&self) -> Rect {
        let f = self.field_rect();
        Rect::new(0, f.bottom() + 14, self.width, 24)
    }

    fn user_rect(&self, i: usize) -> Rect {
        let n = users::count() as i32;
        Rect::new(24, self.height - 24 - (n - i as i32) * 60, 280, 56)
    }

    fn power_rect(&self) -> Rect {
        Rect::new(self.width - 72, self.height - 72, 48, 48)
    }

    fn flyout_rect(&self) -> Rect {
        let p = self.power_rect();
        Rect::new(p.right() - 180, p.y - 96, 180, 88)
    }

    fn target_rect(&self, t: Target) -> Rect {
        let f = self.flyout_rect();
        match t {
            Target::User(i) => self.user_rect(i),
            Target::Submit => self.submit_rect(),
            Target::Power => self.power_rect(),
            Target::Restart => Rect::new(f.x + 4, f.y + 4, f.w - 8, 38),
            Target::ShutDown => Rect::new(f.x + 4, f.y + 46, f.w - 8, 38),
        }
    }

    fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        if !self.panel_shown() {
            return None;
        }
        // the power flyout's items only while it is open
        let fixed = [
            Target::Restart,
            Target::ShutDown,
            Target::Power,
            Target::Submit,
        ];
        let first = if self.power_open { 0 } else { 2 };
        let users = (0..users::count()).map(Target::User);
        fixed[first..]
            .iter()
            .copied()
            .chain(users)
            .find(|&t| self.target_rect(t).contains(x, y))
    }

    fn shake_offset(&self) -> i32 {
        let Some(start) = self.shake else {
            return 0;
        };
        let elapsed = (interrupts::ticks() - start).min(SHAKE_TICKS) as i32;
        let t = elapsed * ONE / SHAKE_TICKS as i32;
        // three swings that die down
        let phase = (t * 6) % (2 * ONE);
        let wave = if phase < ONE {
            ONE - 2 * (phase - ONE / 2).abs()
        } else {
            -(ONE - 2 * (phase - 3 * ONE / 2).abs())
        };
        wave * 14 * (ONE - t) / ONE / ONE
    }

    // ---- animation ---------------------------------------------------------

    /// Move animations on and mark what they changed.
    pub fn tick(&mut self) {
        let moving = !self.reveal.done();
        if moving || core::mem::replace(&mut self.sliding, moving) {
            self.dirty.add(self.screen());
        }
        if self.hover.tick() {
            for t in self.hover.lit().into_iter().flatten() {
                self.dirty.add(self.target_rect(t));
            }
        }
        if let Some(start) = self.shake {
            self.dirty.add(self.field_area());
            if interrupts::ticks() - start >= SHAKE_TICKS {
                self.shake = None;
            }
        }
        let now = interrupts::ticks();
        if self.panel_shown() && now >= self.next_blink {
            self.next_blink = now + BLINK_TICKS;
            self.caret_on = !self.caret_on;
            self.dirty.add(self.field_rect());
        }
    }

    // ---- input -------------------------------------------------------------

    fn changed_panel(&mut self) {
        self.dirty.add(self.field_area());
        self.dirty.add(self.message_rect());
        self.caret_on = true;
        self.next_blink = interrupts::ticks() + BLINK_TICKS;
    }

    pub fn on_key(&mut self, key: Key) -> Outcome {
        if let Key::LayoutChanged = key {
            self.dirty.add(self.power_rect().offset(-64, 0));
            return Outcome::None;
        }
        if !self.panel_shown() {
            // the key that wakes the lock screen only shows the panel
            self.show_panel();
            return Outcome::None;
        }
        match key {
            Key::Char(c) if !c.is_control() && self.password.len() + c.len_utf8() < 64 => {
                let mut buf = [0u8; 4];
                self.password.push_str(c.encode_utf8(&mut buf));
                self.wrong = false;
                self.changed_panel();
            }
            Key::Backspace => {
                self.password.pop();
                self.changed_panel();
            }
            Key::Enter => return self.submit(),
            Key::Escape => {
                if self.power_open {
                    self.power_open = false;
                    self.dirty.add(self.flyout_rect());
                } else {
                    self.show_lock();
                }
            }
            Key::Up | Key::Down => {
                let n = users::count();
                let next = if let Key::Up = key {
                    self.selected + n - 1
                } else {
                    self.selected + 1
                };
                self.select(next % n);
            }
            _ => {}
        }
        Outcome::None
    }

    fn select(&mut self, i: usize) {
        if i != self.selected {
            self.dirty.add(self.user_rect(self.selected));
            self.dirty.add(self.user_rect(i));
            self.dirty.add(self.name_rect());
            self.selected = i;
            self.password.clear();
            self.wrong = false;
            self.changed_panel();
        }
    }

    fn submit(&mut self) -> Outcome {
        if users::sign_in(self.selected, self.password.as_str()) {
            serial::write_str("login: signed in as ");
            if let Some(name) = users::name(self.selected) {
                serial::write_str(name.as_str());
            }
            serial::write_str("\n");
            self.password.clear();
            self.wrong = false;
            return Outcome::SignedIn;
        }
        serial::write_str("login: wrong password\n");
        self.password.clear();
        self.wrong = true;
        self.shake = Some(interrupts::ticks());
        self.changed_panel();
        Outcome::None
    }

    pub fn on_move(&mut self, x: i32, y: i32) {
        let target = self.target_at(x, y);
        if self.hover.set(target) {
            for t in self.hover.lit().into_iter().flatten() {
                self.dirty.add(self.target_rect(t));
            }
        }
    }

    pub fn on_click(&mut self, x: i32, y: i32) -> Outcome {
        if !self.panel_shown() {
            self.show_panel();
            return Outcome::None;
        }
        let target = self.target_at(x, y);
        if self.power_open && !matches!(target, Some(Target::Restart | Target::ShutDown)) {
            self.power_open = false;
            self.dirty.add(self.flyout_rect().union(&self.power_rect()));
            return Outcome::None;
        }
        match target {
            Some(Target::User(i)) => self.select(i),
            Some(Target::Submit) => return self.submit(),
            Some(Target::Power) => {
                self.power_open = true;
                self.dirty.add(self.flyout_rect().union(&self.power_rect()));
            }
            Some(Target::Restart) => return Outcome::Restart,
            Some(Target::ShutDown) => return Outcome::ShutDown,
            None => {}
        }
        Outcome::None
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, wallpaper: &[u32]) {
        let off = anim::lerp(0, self.height, anim::ease_in_out(self.reveal.value()));
        // the lock screen above `split`, the panel below it
        let split = self.height - off;
        if split > 0 {
            let mut lock = c.sub(self.screen());
            lock.clip_to(Rect::new(0, 0, self.width, split));
            self.draw_lock(&mut lock, wallpaper, -off);
        }
        if split < self.height {
            let mut panel = c.sub(self.screen());
            panel.clip_to(Rect::new(0, split, self.width, off));
            self.draw_panel(&mut panel);
        }
    }

    fn draw_lock(&self, c: &mut Canvas, wallpaper: &[u32], dy: i32) {
        c.blit(
            0,
            dy,
            self.width,
            self.height,
            wallpaper,
            self.width as usize,
        );
        let clock = self.clock_rect().offset(0, dy);
        let shade = rgb(0x06, 0x18, 0x48);
        c.text_centered_in(&CLOCK, clock.offset(2, 3), self.clock.as_str(), shade);
        c.text_centered_in(&CLOCK, clock, self.clock.as_str(), WHITE);
        let date = self.date_rect().offset(0, dy);
        c.text_centered_in(&HEADING, date.offset(1, 2), self.date.as_str(), shade);
        c.text_centered_in(&HEADING, date, self.date.as_str(), WHITE);
    }

    fn draw_panel(&self, c: &mut Canvas) {
        c.blit(
            0,
            0,
            self.width,
            self.height,
            self.backdrop,
            self.width as usize,
        );
        let a = self.avatar_rect();
        c.blit_alpha(a.x, a.y, a.w, a.h, &self.big_avatar);
        if let Some(name) = users::name(self.selected) {
            c.text_centered_in(&HEADING, self.name_rect(), name.as_str(), WHITE);
        }
        self.draw_field(c);

        let message = if self.wrong {
            Some(("The password is incorrect. Try again.", WHITE))
        } else if !users::has_password(self.selected) {
            let dim = rgb(0xc8, 0xd2, 0xe4);
            Some(("No password is set: press Enter to sign in", dim))
        } else {
            None
        };
        if let Some((text, color)) = message {
            c.text_centered(self.message_rect(), text, color);
        }

        for i in 0..users::count() {
            let r = self.user_rect(i);
            if !c.visible(r) {
                continue;
            }
            let lit = if i == self.selected { 56 } else { 0 };
            let lit = lit + self.hover.level(Target::User(i)) * 36 / ONE;
            if lit > 0 {
                c.fill_round_alpha(r, 6, WHITE, lit);
            }
            let s = SMALL_AVATAR;
            c.blit_alpha(r.x + 10, r.y + (r.h - s) / 2, s, s, &self.small_avatar);
            if let Some(name) = users::name(i) {
                let y = r.y + (r.h - UI.line_height) / 2;
                c.draw_text(r.x + 58, y, name.as_str(), WHITE);
            }
        }

        let p = self.power_rect();
        let layout = Rect::new(p.x - 56, p.y, 48, p.h);
        c.text_centered(layout, self.layout, WHITE);
        let lit = self.hover.level(Target::Power) * 60 / ONE + if self.power_open { 40 } else { 0 };
        if lit > 0 {
            c.fill_round_alpha(p, 6, WHITE, lit);
        }
        // the gap in the ring is painted with the colour behind it
        let behind = self.backdrop[(p.y + 10) as usize * self.width as usize + p.x as usize];
        power_symbol(
            c,
            p.x + 24,
            p.y + 24,
            WHITE,
            fast_mix(behind, WHITE, lit as u32),
        );
        if self.power_open {
            self.draw_flyout(c);
        }
    }

    fn draw_field(&self, c: &mut Canvas) {
        let f = self.field_rect();
        c.fill_round_alpha(f, 4, rgb(0xfb, 0xfb, 0xfd), 240);
        c.fill_rect(f.x + 2, f.bottom() - 2, f.w - 4, 2, theme::accent());
        let ty = f.y + (f.h - UI.line_height) / 2;
        let text_x = f.x + 12;
        let mut end = text_x;
        if self.password.len() == 0 {
            c.draw_text(text_x, ty, "Password", theme::text_dim());
        } else {
            // a dot per character
            for i in 0..self.password.as_str().chars().count() as i32 {
                let x = text_x + i * 12;
                if x + 8 > f.right() - 40 {
                    break;
                }
                c.fill_round(Rect::new(x, f.y + f.h / 2 - 4, 8, 8), 4, theme::text());
                end = x + 10;
            }
        }
        if self.caret_on && self.panel_shown() {
            c.fill_rect(end.max(text_x) + 1, ty, 1, UI.line_height, theme::text());
        }
        // the arrow button
        let s = self.submit_rect();
        let lit = self.hover.level(Target::Submit);
        c.fill_round(
            s,
            4,
            mix(
                rgb(0xe8, 0xea, 0xf0),
                theme::accent(),
                lit as u32 * 255 / 256,
            ),
        );
        let arrow = mix(theme::text(), WHITE, lit as u32 * 255 / 256);
        let (cx, cy) = (s.x + s.w / 2, s.y + s.h / 2);
        c.fill_rect(cx - 7, cy - 1, 13, 2, arrow);
        for i in 0..6 {
            c.fill_rect(cx + i - 1, cy - 6 + i, 2, 1, arrow);
            c.fill_rect(cx + i - 1, cy + 5 - i, 2, 1, arrow);
        }
    }

    fn draw_flyout(&self, c: &mut Canvas) {
        let f = self.flyout_rect();
        c.shadow(f, 8, 10, 2, 90);
        c.fill_round(f, 8, rgb(0xfb, 0xfb, 0xfd));
        c.outline_round(f, 8, theme::stroke());
        for t in [Target::Restart, Target::ShutDown] {
            let r = self.target_rect(t);
            let lit = self.hover.level(t) as u32;
            let bg = mix(
                rgb(0xfb, 0xfb, 0xfd),
                theme::accent_light(),
                lit * 255 / 256,
            );
            if lit > 0 {
                c.fill_round(r, 5, bg);
            }
            if t == Target::Restart {
                restart_symbol(c, r.x + 20, r.y + 19, theme::text(), bg);
                c.draw_text(r.x + 40, r.y + 10, "Restart", theme::text());
            } else {
                power_symbol(c, r.x + 20, r.y + 19, theme::text(), bg);
                c.draw_text(r.x + 40, r.y + 10, "Shut down", theme::text());
            }
        }
    }
}

// ---- pictures -----------------------------------------------------------------

/// The default user picture: a light silhouette on a grey-blue disc,
/// sampled 4x4 per pixel for smooth edges. Alpha is in the top byte.
fn avatar(size: i32) -> Vec<u32> {
    const S: i64 = 4;
    let n = size as i64 * S;
    let r = n / 2;
    // head and shoulders, in sample units
    let (hx, hy, hr) = (n / 2, n * 40 / 100, n * 19 / 100);
    let (sx, sy, srx, sry) = (n / 2, n * 98 / 100, n * 36 / 100, n * 32 / 100);
    let disc = rgb(0x9a, 0xa8, 0xbe);
    let figure = rgb(0xe8, 0xee, 0xf6);
    let mut out = vec![0u32; (size * size) as usize];
    for (i, px) in out.iter_mut().enumerate() {
        let (ox, oy) = (i as i64 % size as i64, i as i64 / size as i64);
        let (mut inside, mut lit) = (0u32, 0u32);
        for y in oy * S..oy * S + S {
            for x in ox * S..ox * S + S {
                let (dx, dy) = (2 * x + 1 - 2 * r, 2 * y + 1 - 2 * r);
                if dx * dx + dy * dy > 4 * r * r {
                    continue;
                }
                inside += 1;
                let (hdx, hdy) = (x - hx, y - hy);
                let head = hdx * hdx + hdy * hdy <= hr * hr;
                let (bx, by) = (x - sx, y - sy);
                let body = bx * bx * sry * sry + by * by * srx * srx <= srx * srx * sry * sry;
                if head || body {
                    lit += 1;
                }
            }
        }
        if let Some(amount) = (lit * 255).checked_div(inside) {
            let color = mix(disc, figure, amount);
            let alpha = inside * 255 / (S * S) as u32;
            *px = alpha << 24 | color;
        }
    }
    out
}

/// Blur the wallpaper and darken it for the panel. The blur runs on a
/// copy an eighth the size, so it costs almost nothing, and is then
/// stretched back smoothly.
fn make_backdrop(wallpaper: &[u32], out: &mut [u32], w: usize, h: usize) {
    const F: usize = 8;
    let (sw, sh) = ((w / F).max(1), (h / F).max(1));
    let mut small = vec![[0u32; 3]; sw * sh];
    for (i, px) in small.iter_mut().enumerate() {
        let (sx, sy) = (i % sw, i / sw);
        let mut sum = [0u32; 3];
        for y in sy * F..(sy * F + F).min(h) {
            for x in sx * F..(sx * F + F).min(w) {
                let p = wallpaper[y * w + x];
                sum[0] += (p >> 16) & 0xff;
                sum[1] += (p >> 8) & 0xff;
                sum[2] += p & 0xff;
            }
        }
        *px = sum.map(|s| s / (F * F) as u32);
    }
    let mut tmp = small.clone();
    for _ in 0..2 {
        box_blur(&small, &mut tmp, sw, sh, 1, sw, 3);
        box_blur(&tmp, &mut small, sh, sw, sw, 1, 3);
    }
    let shade = rgb(0x04, 0x0c, 0x22);
    for y in 0..h {
        // sample position in small pixels, 1/256 steps
        let fy = ((y * 256 + 128) / F).saturating_sub(128);
        let (y0, ty) = ((fy / 256).min(sh - 1), (fy % 256) as u32);
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let fx = ((x * 256 + 128) / F).saturating_sub(128);
            let (x0, tx) = ((fx / 256).min(sw - 1), (fx % 256) as u32);
            let x1 = (x0 + 1).min(sw - 1);
            let mut c = 0u32;
            for (ch, shift) in [16u32, 8, 0].into_iter().enumerate() {
                let top = small[y0 * sw + x0][ch] * (256 - tx) + small[y0 * sw + x1][ch] * tx;
                let bottom = small[y1 * sw + x0][ch] * (256 - tx) + small[y1 * sw + x1][ch] * tx;
                let v = (top * (256 - ty) + bottom * ty) >> 16;
                c |= v.min(255) << shift;
            }
            out[y * w + x] = fast_mix(c, shade, 96);
        }
    }
}

/// One pass of a box blur along lines of `len` pixels. `step` moves
/// along a line and `line_step` to the next one.
fn box_blur(
    src: &[[u32; 3]],
    dst: &mut [[u32; 3]],
    len: usize,
    lines: usize,
    step: usize,
    line_step: usize,
    radius: usize,
) {
    let width = 2 * radius as u32 + 1;
    for line in 0..lines {
        let at = |i: isize| {
            let i = i.clamp(0, len as isize - 1) as usize;
            line * line_step + i * step
        };
        let mut sum = [0u32; 3];
        for i in -(radius as isize)..=radius as isize {
            for (ch, s) in sum.iter_mut().enumerate() {
                *s += src[at(i)][ch];
            }
        }
        for i in 0..len as isize {
            dst[at(i)] = sum.map(|s| s / width);
            let (add, sub) = (at(i + radius as isize + 1), at(i - radius as isize));
            for (ch, s) in sum.iter_mut().enumerate() {
                *s = *s + src[add][ch] - src[sub][ch];
            }
        }
    }
}
