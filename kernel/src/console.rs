//! The screen: a text console with colours, scrolling and a blinking
//! cursor, plus a status bar and mouse pointer in graphics mode.
//!
//! In graphics mode (framebuffer from GRUB) text is drawn with the bitmap
//! font. If GRUB left us in VGA text mode, the console writes to the VGA
//! text buffer at 0xb8000 instead. Everything printed is also mirrored
//! to the serial port.

use core::fmt;

use crate::font;
use crate::framebuffer::{Framebuffer, Rgb};
use crate::serial;
use crate::sync::IrqMutex;
use crate::vga;

/// The 16 classic VGA colours.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Color {
    #[default]
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

impl Color {
    /// The colour used for this palette entry in graphics mode.
    pub fn rgb(self) -> Rgb {
        const PALETTE: [Rgb; 16] = [
            Rgb::new(0x12, 0x14, 0x1c),
            Rgb::new(0x30, 0x5f, 0xc8),
            Rgb::new(0x3c, 0xa0, 0x50),
            Rgb::new(0x2a, 0xa1, 0xb3),
            Rgb::new(0xc8, 0x3c, 0x3c),
            Rgb::new(0xa0, 0x50, 0xb4),
            Rgb::new(0xc0, 0x8a, 0x30),
            Rgb::new(0xc0, 0xc4, 0xcc),
            Rgb::new(0x5a, 0x60, 0x6e),
            Rgb::new(0x6c, 0x9c, 0xff),
            Rgb::new(0x7c, 0xe0, 0x8c),
            Rgb::new(0x70, 0xe0, 0xf0),
            Rgb::new(0xff, 0x6e, 0x6e),
            Rgb::new(0xf0, 0x8c, 0xf0),
            Rgb::new(0xff, 0xe0, 0x6e),
            Rgb::new(0xf4, 0xf6, 0xfa),
        ];
        PALETTE[self as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Cell {
    /// '\0' means an empty cell.
    ch: char,
    fg: Color,
    bg: Color,
}

enum Surface {
    /// Not initialised yet: output only goes to the serial port.
    None,
    Text,
    Graphics(Framebuffer),
    /// The desktop draws the text in a window; the console only keeps
    /// the cells. The framebuffer is kept for the panic screen.
    Offscreen(Framebuffer),
}

/// Height of the status bar at the top of the screen in graphics mode.
const BAR_HEIGHT: usize = 24;
const MAX_COLS: usize = 256;
const MAX_ROWS: usize = 128;
const TAB_WIDTH: usize = 4;

/// Mouse pointer: 'X' is the outline, '.' the fill, ' ' transparent.
const POINTER: [&[u8; 12]; 19] = [
    b"X           ",
    b"XX          ",
    b"X.X         ",
    b"X..X        ",
    b"X...X       ",
    b"X....X      ",
    b"X.....X     ",
    b"X......X    ",
    b"X.......X   ",
    b"X........X  ",
    b"X.........X ",
    b"X......XXXXX",
    b"X...X..X    ",
    b"X..XX..X    ",
    b"X.X  X..X   ",
    b"XX   X..X   ",
    b"X     X..X  ",
    b"      X..X  ",
    b"       XX   ",
];
const POINTER_W: usize = 12;
const POINTER_H: usize = 19;

struct Mouse {
    x: usize,
    y: usize,
    shown: bool,
    /// Pixels under the pointer, restored when it moves.
    saved: [u32; POINTER_W * POINTER_H],
}

pub struct Console {
    surface: Surface,
    cols: usize,
    rows: usize,
    /// First pixel row of the text area (below the status bar).
    origin_y: usize,
    cells: [Cell; MAX_COLS * MAX_ROWS],
    col: usize,
    row: usize,
    fg: Color,
    bg: Color,
    /// Blink phase of the text cursor.
    cursor_on: bool,
    mouse: Mouse,
    status: [char; 64],
    status_len: usize,
    /// The text changed since `take_changed` was last called.
    changed: bool,
}

pub static CONSOLE: IrqMutex<Console> = IrqMutex::new(Console::new());

impl Console {
    const fn new() -> Self {
        Self {
            surface: Surface::None,
            cols: 0,
            rows: 0,
            origin_y: 0,
            cells: [Cell {
                ch: '\0',
                fg: Color::Black,
                bg: Color::Black,
            }; MAX_COLS * MAX_ROWS],
            col: 0,
            row: 0,
            fg: Color::LightGray,
            bg: Color::Black,
            cursor_on: true,
            mouse: Mouse {
                x: 0,
                y: 0,
                shown: false,
                saved: [0; POINTER_W * POINTER_H],
            },
            status: ['\0'; 64],
            status_len: 0,
            changed: false,
        }
    }

    /// Start drawing on the framebuffer, or on the VGA text buffer when
    /// there is none.
    pub fn init(&mut self, framebuffer: Option<Framebuffer>) {
        match framebuffer {
            Some(fb) => {
                self.origin_y = BAR_HEIGHT;
                self.cols = (fb.width / font::WIDTH).min(MAX_COLS);
                self.rows = ((fb.height - BAR_HEIGHT) / font::HEIGHT).min(MAX_ROWS);
                self.mouse.x = fb.width / 2;
                self.mouse.y = fb.height / 2;
                self.surface = Surface::Graphics(fb);
            }
            None => {
                self.origin_y = 0;
                self.cols = vga::WIDTH;
                self.rows = vga::HEIGHT;
                self.mouse.x = vga::WIDTH * font::WIDTH / 2;
                self.mouse.y = vga::HEIGHT * font::HEIGHT / 2;
                self.surface = Surface::Text;
                vga::enable_cursor();
            }
        }
        if let Surface::Graphics(fb) = &self.surface {
            // paint the gap below the last text row too
            fb.fill_rect(0, 0, fb.width, fb.height, Color::Black.rgb());
        }
        self.clear();
        self.draw_status_bar();
        self.show_mouse();
    }

    /// Stop drawing on the screen and keep a `cols` x `rows` text area in
    /// memory instead, for the desktop's terminal window.
    pub fn detach(&mut self, cols: usize, rows: usize) {
        if let Surface::Graphics(fb) = self.surface {
            self.hide_mouse();
            self.surface = Surface::Offscreen(fb);
            self.cols = cols.min(MAX_COLS);
            self.rows = rows.min(MAX_ROWS);
            self.clear();
        }
    }

    /// Take the screen back from the desktop, for the panic screen.
    pub fn reattach(&mut self) {
        if let Surface::Offscreen(fb) = self.surface {
            self.init(Some(fb));
        }
    }

    pub fn take_changed(&mut self) -> bool {
        core::mem::replace(&mut self.changed, false)
    }

    /// Character and colours of a cell; empty cells are spaces.
    pub fn cell(&self, row: usize, col: usize) -> (char, Color, Color) {
        let cell = self.cells[row * self.cols + col];
        let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
        (ch, cell.fg, cell.bg)
    }

    /// Text cursor as (row, column).
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Current text colour.
    pub fn color(&self) -> Color {
        self.fg
    }

    /// Text area size in characters.
    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn set_color(&mut self, fg: Color, bg: Color) {
        self.fg = fg;
        self.bg = bg;
    }

    pub fn clear(&mut self) {
        self.edit(|con| {
            let blank = con.blank();
            for i in 0..con.cols * con.rows {
                con.cells[i] = blank;
            }
            for row in 0..con.rows {
                for col in 0..con.cols {
                    con.draw_cell(row, col);
                }
            }
            con.row = 0;
            con.col = 0;
        });
    }

    /// Erase the character before the cursor, going back a line if needed.
    pub fn backspace(&mut self) {
        self.edit(|con| {
            if con.col > 0 {
                con.col -= 1;
            } else if con.row > 0 {
                con.row -= 1;
                con.col = con.cols - 1;
            } else {
                return;
            }
            let (row, col) = (con.row, con.col);
            con.cells[row * con.cols + col] = con.blank();
            con.draw_cell(row, col);
        });
    }

    /// Flip the blinking text cursor. Called by the main loop on a timer.
    pub fn blink(&mut self) {
        if self.cols == 0 {
            return;
        }
        self.hide_mouse();
        self.cursor_on = !self.cursor_on;
        self.draw_cell(self.row, self.col);
        self.show_mouse();
    }

    /// Move the mouse pointer by a relative amount (screen y grows down).
    pub fn move_mouse(&mut self, dx: i32, dy: i32) -> (usize, usize) {
        let (w, h) = match &self.surface {
            Surface::Graphics(fb) => (fb.width, fb.height),
            Surface::Text => (vga::WIDTH * font::WIDTH, vga::HEIGHT * font::HEIGHT),
            Surface::None | Surface::Offscreen(_) => return (0, 0),
        };
        self.hide_mouse();
        self.mouse.x = (self.mouse.x as i32 + dx).clamp(0, w as i32 - 1) as usize;
        self.mouse.y = (self.mouse.y as i32 + dy).clamp(0, h as i32 - 1) as usize;
        self.show_mouse();
        (self.mouse.x, self.mouse.y)
    }

    pub fn mouse_position(&self) -> (usize, usize) {
        (self.mouse.x, self.mouse.y)
    }

    /// Set the text on the right side of the status bar.
    pub fn set_status(&mut self, text: &str) {
        self.status_len = 0;
        for c in text.chars().take(self.status.len()) {
            self.status[self.status_len] = c;
            self.status_len += 1;
        }
        self.hide_mouse();
        self.draw_status_bar();
        self.show_mouse();
    }

    // ---- internals -----------------------------------------------------

    fn blank(&self) -> Cell {
        Cell {
            ch: '\0',
            fg: self.fg,
            bg: self.bg,
        }
    }

    /// Run a change to the text with the cursor and mouse pointer taken
    /// off the screen, then put them back.
    fn edit(&mut self, change: impl FnOnce(&mut Self)) {
        if self.cols == 0 {
            return;
        }
        self.hide_mouse();
        self.cursor_on = false;
        self.draw_cell(self.row, self.col);
        change(self);
        self.changed = true;
        self.cursor_on = true;
        self.draw_cell(self.row, self.col);
        if let Surface::Text = self.surface {
            vga::set_cursor(self.row, self.col);
        }
        self.show_mouse();
    }

    fn put_char(&mut self, c: char) {
        match c {
            '\n' => self.new_line(),
            '\r' => self.col = 0,
            '\t' => {
                let spaces = TAB_WIDTH - self.col % TAB_WIDTH;
                for _ in 0..spaces {
                    self.put_char(' ');
                }
            }
            c => {
                if self.col >= self.cols {
                    self.new_line();
                }
                let (row, col) = (self.row, self.col);
                self.cells[row * self.cols + col] = Cell {
                    ch: c,
                    fg: self.fg,
                    bg: self.bg,
                };
                self.draw_cell(row, col);
                self.col += 1;
            }
        }
    }

    fn new_line(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Move all lines up by one. Only cells that change are redrawn,
    /// which keeps scrolling fast when much of the screen is empty.
    fn scroll(&mut self) {
        let cols = self.cols;
        for row in 0..self.rows {
            for col in 0..cols {
                let below = if row + 1 < self.rows {
                    self.cells[(row + 1) * cols + col]
                } else {
                    self.blank()
                };
                if self.cells[row * cols + col] != below {
                    self.cells[row * cols + col] = below;
                    self.draw_cell(row, col);
                }
            }
        }
    }

    fn draw_cell(&self, row: usize, col: usize) {
        // the cursor sits one past the last column until the next
        // character wraps the line
        if col >= self.cols {
            return;
        }
        let cell = self.cells[row * self.cols + col];
        let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
        match &self.surface {
            Surface::None | Surface::Offscreen(_) => {}
            Surface::Text => vga::put(row, col, ch, cell.fg as u8, cell.bg as u8),
            Surface::Graphics(fb) => {
                let (x, y) = (col * font::WIDTH, self.origin_y + row * font::HEIGHT);
                fb.draw_char(x, y, ch, cell.fg.rgb(), Some(cell.bg.rgb()));
                if self.cursor_on && (row, col) == (self.row, self.col) {
                    fb.fill_rect(x, y + font::HEIGHT - 3, font::WIDTH, 2, self.fg.rgb());
                }
            }
        }
    }

    fn draw_status_bar(&self) {
        let Surface::Graphics(fb) = &self.surface else {
            return;
        };
        fb.horizontal_gradient(
            0,
            0,
            fb.width,
            BAR_HEIGHT,
            Rgb::new(0x2a, 0x3c, 0x8c),
            Rgb::new(0x6a, 0x2c, 0x8c),
        );
        fb.fill_rect(0, BAR_HEIGHT - 1, fb.width, 1, Rgb::new(0x9a, 0x8c, 0xff));
        fb.fill_circle(14, 12, 6, Color::Yellow.rgb());
        fb.fill_circle(14, 12, 3, Rgb::new(0x2a, 0x3c, 0x8c));
        fb.draw_text(28, 4, "RyzikOS", Color::White.rgb(), None);
        let x = fb.width.saturating_sub((self.status_len + 1) * font::WIDTH);
        for (i, &c) in self.status[..self.status_len].iter().enumerate() {
            fb.draw_char(x + i * font::WIDTH, 4, c, Rgb::new(0xdc, 0xe0, 0xff), None);
        }
    }

    fn show_mouse(&mut self) {
        if self.mouse.shown {
            return;
        }
        match &self.surface {
            Surface::None | Surface::Offscreen(_) => return,
            Surface::Text => {
                // show the pointer as an inverted character cell
                let (row, col) = (self.mouse.y / font::HEIGHT, self.mouse.x / font::WIDTH);
                let cell = self.cells[row * self.cols + col];
                let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                vga::put(row, col, ch, cell.bg as u8, cell.fg as u8);
            }
            Surface::Graphics(fb) => {
                let (outline, fill) = (
                    fb.encode(Rgb::new(0, 0, 0)),
                    fb.encode(Rgb::new(255, 255, 255)),
                );
                for (py, line) in POINTER.iter().enumerate() {
                    for (px, &p) in line.iter().enumerate() {
                        let (x, y) = (self.mouse.x + px, self.mouse.y + py);
                        self.mouse.saved[py * POINTER_W + px] = fb.get_raw(x, y);
                        match p {
                            b'X' => fb.put_raw(x, y, outline),
                            b'.' => fb.put_raw(x, y, fill),
                            _ => {}
                        }
                    }
                }
            }
        }
        self.mouse.shown = true;
    }

    fn hide_mouse(&mut self) {
        if !self.mouse.shown {
            return;
        }
        self.mouse.shown = false;
        match &self.surface {
            Surface::None | Surface::Offscreen(_) => {}
            Surface::Text => {
                self.draw_cell(self.mouse.y / font::HEIGHT, self.mouse.x / font::WIDTH)
            }
            Surface::Graphics(fb) => {
                for py in 0..POINTER_H {
                    for px in 0..POINTER_W {
                        let value = self.mouse.saved[py * POINTER_W + px];
                        fb.put_raw(self.mouse.x + px, self.mouse.y + py, value);
                    }
                }
            }
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        serial::write_str(text);
        self.edit(|con| text.chars().for_each(|c| con.put_char(c)));
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    let _ = CONSOLE.lock().write_fmt(args);
}

/// Print in a colour, then switch back to the previous one.
pub fn print_colored(fg: Color, args: fmt::Arguments) {
    use core::fmt::Write;
    let mut con = CONSOLE.lock();
    let (old_fg, old_bg) = (con.fg, con.bg);
    con.fg = fg;
    let _ = con.write_fmt(args);
    con.set_color(old_fg, old_bg);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::console::_print(format_args!("{}\n", format_args!($($arg)*))));
}
