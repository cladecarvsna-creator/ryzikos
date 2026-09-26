//! Timing for animations. Everything runs off the 100 Hz timer: an
//! animation knows when it started and how long it lasts, and each frame
//! asks where it is now. A slow frame skips ahead instead of slowing the
//! animation down.

use crate::interrupts;

/// Full scale for progress and opacity values.
pub const ONE: i32 = 256;

pub fn ms(n: u64) -> u64 {
    (n * interrupts::TIMER_HZ / 1000).max(1)
}

/// Fast start, soft landing.
pub fn ease_out(t: i32) -> i32 {
    let u = ONE - t.clamp(0, ONE);
    ONE - u * u / ONE * u / ONE
}

/// Soft start and landing.
pub fn ease_in_out(t: i32) -> i32 {
    let t = t.clamp(0, ONE);
    if t < ONE / 2 {
        4 * t * t / ONE * t / ONE
    } else {
        let u = ONE - t;
        ONE - 4 * u * u / ONE * u / ONE
    }
}

pub fn lerp(a: i32, b: i32, t: i32) -> i32 {
    a + (b - a) * t / ONE
}

/// A value moving from `from` to `to` (both 0 to ONE) with ease-out.
#[derive(Clone, Copy)]
pub struct Tween {
    from: i32,
    to: i32,
    start: u64,
    duration: u64,
}

impl Tween {
    pub fn new(from: i32, to: i32, duration: u64) -> Self {
        Self {
            from,
            to,
            start: interrupts::ticks(),
            duration: duration.max(1),
        }
    }

    /// Head for `to`, starting from wherever the value is now, so a
    /// reversed animation does not jump.
    pub fn retarget(&mut self, to: i32, duration: u64) {
        let now = self.value();
        // go only as long as the remaining distance needs
        let distance = (to - now).unsigned_abs() as u64;
        *self = Self::new(now, to, (duration * distance / ONE as u64).max(1));
    }

    fn t(&self) -> i32 {
        let elapsed = interrupts::ticks() - self.start;
        (elapsed * ONE as u64 / self.duration).min(ONE as u64) as i32
    }

    pub fn value(&self) -> i32 {
        lerp(self.from, self.to, ease_out(self.t()))
    }

    pub fn done(&self) -> bool {
        self.t() >= ONE
    }

    pub fn target(&self) -> i32 {
        self.to
    }
}

/// A highlight that fades in on the thing under the mouse and fades out
/// on the one it left.
#[derive(Clone, Copy)]
pub struct Fader<T: Copy + PartialEq> {
    current: Option<T>,
    level: i32,
    previous: Option<T>,
    previous_level: i32,
    last_tick: u64,
    /// Ticks to go from off to fully on.
    speed: u64,
}

impl<T: Copy + PartialEq> Fader<T> {
    pub fn new(speed: u64) -> Self {
        Self {
            current: None,
            level: 0,
            previous: None,
            previous_level: 0,
            last_tick: 0,
            speed: speed.max(1),
        }
    }

    /// Point at something else. Returns whether anything changed.
    pub fn set(&mut self, target: Option<T>) -> bool {
        if target == self.current {
            return false;
        }
        if target.is_some() && target == self.previous {
            // back to the one still fading out: continue from its level
            core::mem::swap(&mut self.previous_level, &mut self.level);
        } else {
            self.previous_level = self.level;
            self.level = 0;
        }
        self.previous = self.current;
        self.current = target;
        self.last_tick = interrupts::ticks();
        true
    }

    /// Set without fading, for things that appear already highlighted.
    pub fn jump(&mut self, target: Option<T>) {
        self.current = target;
        self.level = if target.is_some() { ONE } else { 0 };
        self.previous = None;
        self.previous_level = 0;
    }

    /// Move the levels on. Returns whether they changed.
    pub fn tick(&mut self) -> bool {
        if !self.busy() {
            return false;
        }
        let now = interrupts::ticks();
        let step = ((now - self.last_tick) * ONE as u64 / self.speed) as i32;
        if step == 0 {
            return false;
        }
        self.last_tick = now;
        if self.current.is_some() {
            self.level = (self.level + step).min(ONE);
        }
        self.previous_level = (self.previous_level - step).max(0);
        if self.previous_level == 0 {
            self.previous = None;
        }
        true
    }

    pub fn busy(&self) -> bool {
        (self.current.is_some() && self.level < ONE) || self.previous.is_some()
    }

    /// How lit `t` is, 0 to ONE.
    pub fn level(&self, t: T) -> i32 {
        if self.current == Some(t) {
            self.level
        } else if self.previous == Some(t) {
            self.previous_level
        } else {
            0
        }
    }

    /// Everything that is lit at all, to redraw.
    pub fn lit(&self) -> [Option<T>; 2] {
        [self.current, self.previous]
    }
}
