//! Starting, restarting and shutting down, with the animations Windows
//! has: the logo with a ring of dots circling under it while EverOS
//! starts, and "Restarting" or "Shutting down" with the same dots over
//! the blurred wallpaper before the power goes.

use super::anim::{self, ease_in_out, Tween, ONE};
use super::canvas::{mix, rgb, Canvas, Rect};
use super::text::{HEADING, UI};
use super::{draw_start_logo_at, Desktop, Phase};
use crate::{interrupts, serial};

/// How long the boot screen shows at least.
const BOOT_MS: u64 = 2600;
/// How long "Restarting" and "Shutting down" show before the power goes.
const POWER_MS: u64 = 2400;
/// One lap of the dots.
const LAP_MS: u64 = 2200;
const DOTS: i32 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
}

/// sin and cos of a whole number of degrees, times 1000.
fn sin_cos(deg: i32) -> (i32, i32) {
    let sin = |d: i32| -> i32 {
        let d = d.rem_euclid(360);
        let (x, sign) = if d < 180 { (d, 1) } else { (d - 180, -1) };
        // Bhaskara's approximation, good to about 0.2%
        let x = x as i64;
        sign * (4000 * x * (180 - x) / (40500 - x * (180 - x))) as i32
    };
    (sin(deg), sin(deg + 90))
}

/// The ring of dots at `cx`, `cy`, `ms` milliseconds into the animation.
/// The dots chase each other: they bunch up at the bottom and spread out
/// as they speed up, twice a lap, the way Windows draws it.
pub fn draw_spinner(c: &mut Canvas, cx: i32, cy: i32, radius: i32, ms: u64, color: u32) {
    for i in 0..DOTS {
        // each dot starts a little after the one before it
        let delay = i as u64 * 150;
        let local = (ms + LAP_MS - delay) % LAP_MS;
        let p = (local * 2 * ONE as u64 / LAP_MS) as i32;
        let (half, t) = (p / ONE, p % ONE);
        let angle = 90 + half * 360 / 2 + ease_in_out(t) * 180 / ONE;
        let (s, co) = sin_cos(angle);
        let (x, y) = (cx + co * radius / 1000, cy + s * radius / 1000);
        let d = 7;
        c.fill_round(Rect::new(x - d / 2, y - d / 2, d, d), d / 2, color);
    }
}

/// The area the spinner draws on.
pub fn spinner_rect(cx: i32, cy: i32, radius: i32) -> Rect {
    Rect::new(
        cx - radius - 6,
        cy - radius - 6,
        2 * radius + 12,
        2 * radius + 12,
    )
}

impl Desktop<'_> {
    fn boot_spinner(&self) -> (i32, i32, i32) {
        (self.width / 2, self.height * 3 / 4, 22)
    }

    /// The boot screen: black, the logo, the dots.
    pub(super) fn draw_boot(&self, c: &mut Canvas, since: u64) {
        c.fill(self.screen(), 0x000000);
        let ms = elapsed_ms(since);
        // the logo fades in first
        let fade = (ms * ONE as u64 / 500).min(ONE as u64) as u32;
        let (lx, ly) = (self.width / 2 - 60, self.height / 2 - 110);
        draw_start_logo_at(c, lx, ly, 5, fade);
        let label = Rect::new(0, ly + 140, self.width, 40);
        c.text_centered_in(
            &HEADING,
            label,
            "EverOS",
            mix(0, 0xffffff, fade * 230 / 256),
        );
        if ms > 400 {
            let (x, y, r) = self.boot_spinner();
            draw_spinner(c, x, y, r, ms - 400, 0xffffff);
        }
    }

    /// Restarting or shutting down: the dots and a word over the blurred
    /// wallpaper.
    pub(super) fn draw_power(&self, c: &mut Canvas, what: Power, since: u64) {
        let (w, h) = (self.width, self.height);
        c.blit(0, 0, w, h, self.login.backdrop(), w as usize);
        let (x, y, r) = self.power_spinner();
        let ms = elapsed_ms(since);
        if ms >= POWER_MS && what == Power::ShutDown {
            // the power did not go off: nothing more is running
            c.fill(self.screen(), 0x000000);
            let msg = "It's now safe to turn off your computer";
            c.text_centered_in(&HEADING, Rect::new(0, h / 2 - 20, w, 40), msg, 0xffffff);
            return;
        }
        draw_spinner(c, x, y, r, ms, 0xffffff);
        let word = match what {
            Power::Restart => "Restarting",
            Power::ShutDown => "Shutting down",
        };
        let text = Rect::new(0, y + r + 28, w, 40);
        c.text_centered_in(&HEADING, text, word, 0xffffff);
        let note = Rect::new(0, y + r + 72, w, 24);
        c.text_centered_in(&UI, note, "Your settings are saved", rgb(0xc8, 0xd0, 0xe0));
    }

    fn power_spinner(&self) -> (i32, i32, i32) {
        (self.width / 2, self.height / 2 - 40, 22)
    }

    /// Restart or shut down, with the animation first.
    pub(super) fn power(&mut self, what: Power) {
        if matches!(self.phase, Phase::Power(..)) {
            return;
        }
        serial::write_str(match what {
            Power::Restart => "\npower: restarting\n",
            Power::ShutDown => "\npower: shutting down\n",
        });
        self.start.open = false;
        self.menu = Tween::new(0, 0, 1);
        self.panel = None;
        self.panel_anim = Tween::new(0, 0, 1);
        self.popup = None;
        self.tip = None;
        self.search.open = false;
        self.tv.open = false;
        self.slide = None;
        self.take_snapshot();
        self.phase = Phase::Power(what, interrupts::ticks());
        self.crossfade = Some(Tween::new(0, ONE, anim::ms(400)));
        self.damage(self.screen());
    }

    /// Move the boot and power screens on; called every frame.
    pub(super) fn tick_power(&mut self) {
        match self.phase {
            Phase::Boot(since) => {
                let (x, y, r) = self.boot_spinner();
                self.damage(spinner_rect(x, y, r));
                if elapsed_ms(since) < 520 {
                    self.damage(self.screen());
                }
                if elapsed_ms(since) >= BOOT_MS {
                    // fade into the lock screen
                    self.take_snapshot();
                    self.phase = Phase::Login;
                    self.login.lock();
                    self.crossfade = Some(Tween::new(0, ONE, anim::ms(500)));
                    self.damage(self.screen());
                }
            }
            Phase::Power(what, since) => {
                let (x, y, r) = self.power_spinner();
                self.damage(spinner_rect(x, y, r));
                let ms = elapsed_ms(since);
                // a few tries, then the safe-to-turn-off screen stays
                if (POWER_MS..POWER_MS + 200).contains(&ms) && self.crossfade.is_none() {
                    match what {
                        Power::Restart => super::restart(),
                        Power::ShutDown => {
                            super::shut_down();
                            // still here: show the safe-to-turn-off screen
                            self.damage(self.screen());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn elapsed_ms(since: u64) -> u64 {
    (interrupts::ticks() - since) * 1000 / interrupts::TIMER_HZ
}
