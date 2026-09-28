//! Terminal: the text shell in a window. The shell prints to the console
//! as before; the console keeps the text off screen and this window
//! draws it in a smooth monospace font.

use super::canvas::{Canvas, Rect};
use super::text::MONO;
use crate::console::{Color, CONSOLE};
use crate::keyboard::Key;
use crate::multiboot::BootInfo;
use crate::shell::Shell;

pub const COLS: usize = 100;
pub const ROWS: usize = 30;
/// Character cell size.
const CELL_W: i32 = 9;
const CELL_H: i32 = 18;
const PAD: i32 = 8;
pub const CLIENT_W: i32 = COLS as i32 * CELL_W + 2 * PAD;
pub const CLIENT_H: i32 = ROWS as i32 * CELL_H + 2 * PAD;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::Terminal)
}
fn ch() -> i32 {
    super::client_h(super::App::Terminal)
}
/// How many columns and rows fit in the window now.
pub fn grid() -> (usize, usize) {
    (
        ((cw() - 2 * PAD) / CELL_W).max(1) as usize,
        ((ch() - 2 * PAD) / CELL_H).max(1) as usize,
    )
}

/// Background, a little darker than the console's black.
const BACKGROUND: u32 = 0x0c0c10;

pub struct Terminal {
    shell: Shell,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            shell: Shell::new(),
        }
    }

    pub fn start(&mut self) {
        self.shell.prompt();
    }

    pub fn on_key(&mut self, key: Key, boot: &BootInfo) {
        self.shell.on_key(key, boot);
    }

    /// Fit the text area to the window's size now.
    pub fn resized(&mut self) {
        let (cols, rows) = grid();
        CONSOLE.lock().resize(cols, rows);
    }

    pub fn draw(&self, c: &mut Canvas, show_cursor: bool) {
        c.fill_rect(0, 0, cw(), ch(), BACKGROUND);
        let con = CONSOLE.lock();
        let (cols, rows) = con.size();
        for row in 0..rows {
            let y = PAD + row as i32 * CELL_H;
            if !c.visible(Rect::new(0, y, cw(), CELL_H)) {
                continue;
            }
            for col in 0..cols {
                let (ch, fg, bg) = con.cell(row, col);
                let x = PAD + col as i32 * CELL_W;
                if bg != Color::Black {
                    c.fill_rect(x, y, CELL_W, CELL_H, bg.rgb().raw());
                }
                if ch != ' ' && !c.draw_glyph(&MONO, x, y, ch, fg.rgb().raw()) {
                    // not in the smooth font: fall back to the boot font
                    c.draw_bitmap_char(x, y + 1, ch, fg.rgb().raw());
                }
            }
        }
        if show_cursor {
            let (row, col) = con.cursor();
            if col < cols {
                let x = PAD + col as i32 * CELL_W;
                let y = PAD + row as i32 * CELL_H;
                c.fill_rect(x, y + CELL_H - 3, CELL_W, 2, con.color().rgb().raw());
            }
        }
    }
}
