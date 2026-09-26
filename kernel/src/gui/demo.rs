//! A graphics demo window that shows off the drawing functions.

use super::canvas::{mix, rgb, Canvas, Rect};
use crate::console::Color;

pub const CLIENT_W: i32 = 480;
pub const CLIENT_H: i32 = 360;

pub fn draw(c: &mut Canvas) {
    let (w, h) = (c.width, c.height);
    c.vertical_gradient(
        Rect::new(0, 0, w, h),
        rgb(0x10, 0x18, 0x40),
        rgb(0x50, 0x10, 0x40),
    );

    // stars, from a small pseudo-random generator
    let mut seed: u32 = 0x2545_f491;
    for _ in 0..300 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let (x, y) = (seed % w as u32, (seed >> 16) % h as u32);
        c.pixel(x as i32, y as i32, rgb(0xff, 0xff, 0xff));
    }

    // starburst of lines from the centre, ending on the edge of a square
    let (cx, cy) = (w / 2, h / 2);
    let reach = w.min(h) / 2 - 20;
    for i in 0..64 {
        let t = (i % 16) * 2 * reach / 16;
        let (ex, ey) = match i / 16 {
            0 => (-reach + t, -reach),
            1 => (reach, -reach + t),
            2 => (reach - t, reach),
            _ => (-reach, reach - t),
        };
        let color = mix(
            rgb(0x40, (i * 3 + 60) as u8, 0xff),
            rgb(0xff, 0x60, 0xc0),
            (i * 4) as u32,
        );
        c.line(cx, cy, cx + ex, cy + ey, color);
    }

    // palette row
    let size = w / 20;
    for i in 0..16 {
        let x = w / 2 - 8 * size + i * size;
        c.fill_rect(
            x + 2,
            h - size - 30,
            size - 4,
            size - 4,
            palette(i as usize),
        );
    }

    // planets
    c.fill_circle(cx, cy, reach / 4, rgb(0xff, 0xc0, 0x40));
    c.fill_circle(cx, cy, reach / 4 - 8, rgb(0xff, 0xe8, 0x80));
    c.fill_circle(
        cx - reach / 2,
        cy - reach / 3,
        reach / 8,
        rgb(0x60, 0xc0, 0xff),
    );
    c.fill_circle(
        cx + reach / 2,
        cy + reach / 3,
        reach / 10,
        rgb(0xff, 0x70, 0x70),
    );

    c.text_centered(Rect::new(0, 8, w, 16), "EverOS graphics", 0xffffff);
}

fn palette(i: usize) -> u32 {
    const ALL: [Color; 16] = [
        Color::Black,
        Color::Blue,
        Color::Green,
        Color::Cyan,
        Color::Red,
        Color::Magenta,
        Color::Brown,
        Color::LightGray,
        Color::DarkGray,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightCyan,
        Color::LightRed,
        Color::Pink,
        Color::Yellow,
        Color::White,
    ];
    ALL[i].rgb().raw()
}
