//! The RyzikOS desktop: a background with a bloom, icons, windows with
//! rounded corners, round caption buttons and soft shadows, a menu bar
//! along the top with the clock, a floating dock at the bottom with the
//! launcher, and the mouse pointer.
//!
//! Everything is drawn into a back buffer in memory and then copied to
//! the screen, so nothing flickers. Only the areas that changed (the
//! "dirty" rectangles) are redrawn and copied.
//!
//! Before the desktop comes the sign-in screen (login.rs). Windows zoom
//! and fade when they open and close and fly to the taskbar when
//! minimised, the launcher slides up, and highlights fade in and out.
//! Animations follow the timer, and each frame redraws only what moves.
//!
//! The dock and the menu bar (taskbar.rs) have search (search.rs), pinned
//! apps and Task View; windows live on virtual desktops (desktops.rs); the desktop has
//! icons with a selection rectangle and the Recycle Bin (deskicons.rs).

mod about;
mod anim;
mod browser;
mod calc;
mod canvas;
mod deskicons;
mod desktops;
mod explorer;
mod filedialog;
#[rustfmt::skip]
mod font_data;
mod icons;
mod login;
mod notepad;
mod paint;
mod personalize;
mod photos;
mod taskmgr;
mod store;
mod picture;
mod popup;
mod power;
mod search;
mod settings;
mod start;
mod taskbar;
mod terminal;
mod text;
mod theme;
mod tray;
mod video;
mod wallpaper;
#[rustfmt::skip]
pub mod webfont;
mod widgets;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use anim::{lerp, Fader, Tween, ONE};
use canvas::{fast_mix, mix, rgb, Canvas, Dirty, Rect};
use deskicons::DeskIcons;
use desktops::{Switcher, TaskView};
use icons::Icons;
use login::Login;
use popup::{Cmd, Popup};
use search::Search;
use start::StartMenu;
use taskbar::TaskItem;
use tray::{Panel, Tray};

use crate::framebuffer::Framebuffer;
use crate::interrupts::{self, KEYBOARD_BYTES, MOUSE_BYTES};
use crate::keyboard::{self, Key, Keyboard, Layout};
use crate::multiboot::BootInfo;
use crate::sync::{ByteQueue, IrqMutex, StaticBuffer};
use crate::{console::CONSOLE, fs, port, ps2, rtc, serial, users, vmmouse, StackString};

const MAX_W: usize = 1920;
const MAX_H: usize = 1200;
static BACK_BUFFER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The desktop background, drawn once at start.
static WALLPAPER: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// Window contents. Each app draws into its own part only when its content
/// changes, so moving a window just copies pixels.
static SURFACES: StaticBuffer<{ 8 * 1024 * 1024 }> = StaticBuffer::new();
/// The blurred wallpaper behind the sign-in panel.
static BACKDROP: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// The screen being faded away when signing in or locking.
static SNAPSHOT: StaticBuffer<{ MAX_W * MAX_H }> = StaticBuffer::new();
/// Where a zooming window or the sliding start menu is drawn before it
/// is scaled and blended onto the screen.
static SCRATCH: StaticBuffer<{ MAX_W * 1000 }> = StaticBuffer::new();

pub use about::VERSION;

/// Whether the desktop is running (the shell asks before opening apps).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Apps to open or close, sent from the shell.
static REQUESTS: ByteQueue = ByteQueue::new();
const CLOSE: u8 = 0x80;
const LOCK: u8 = 0x40;
const RESTART: u8 = 0x41;
const SHUT_DOWN: u8 = 0x42;

/// The strip at the bottom the dock floats in; windows stay above it.
const TASKBAR_H: i32 = 80;
/// The menu bar along the top of the screen.
const MENUBAR_H: i32 = 30;
const TITLE_H: i32 = 32;
const BORDER: i32 = 1;
const WINDOW_RADIUS: i32 = 8;
/// How far window shadows reach.
const SPREAD: i32 = 16;
const SHADOW_DROP: i32 = 4;
const DOUBLE_CLICK_TICKS: u64 = interrupts::TIMER_HZ / 2;
const BLINK_TICKS: u64 = interrupts::TIMER_HZ / 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum App {
    Terminal,
    Explorer,
    Notepad,
    Paint,
    Calculator,
    Photos,
    Video,
    Store,
    Browser,
    Settings,
    About,
    TaskManager,
    /// A downloaded program (.rzapp) in its own window.
    Program,
}

const APPS: [App; 13] = [
    App::Terminal,
    App::Explorer,
    App::Notepad,
    App::Paint,
    App::Calculator,
    App::Photos,
    App::Video,
    App::Store,
    App::Browser,
    App::Settings,
    App::About,
    App::TaskManager,
    App::Program,
];

impl App {
    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            App::Explorer => "Files",
            App::Notepad => "Text Editor",
            App::Paint => "Draw",
            App::Calculator => "Calculator",
            App::Photos => "Photos",
            App::Video => "Video Player",
            App::Store => "App Store",
            App::Browser => "Browser",
            App::Settings => "Settings",
            App::About => "About RyzikOS",
            App::TaskManager => "Task Manager",
            App::Program => "Program",
        }
    }

    fn client_size(self) -> (i32, i32) {
        match self {
            App::Terminal => (terminal::CLIENT_W, terminal::CLIENT_H),
            App::Explorer => (explorer::CLIENT_W, explorer::CLIENT_H),
            App::Notepad => (notepad::CLIENT_W, notepad::CLIENT_H),
            App::Paint => (paint::CLIENT_W, paint::CLIENT_H),
            App::Calculator => (calc::CLIENT_W, calc::CLIENT_H),
            App::Photos => (photos::CLIENT_W, photos::CLIENT_H),
            App::Video => (video::CLIENT_W, video::CLIENT_H),
            App::Store => (store::CLIENT_W, store::CLIENT_H),
            App::Browser => (browser::CLIENT_W, browser::CLIENT_H),
            App::Settings => (settings::CLIENT_W, settings::CLIENT_H),
            App::About => (about::CLIENT_W, about::CLIENT_H),
            App::TaskManager => (taskmgr::CLIENT_W, taskmgr::CLIENT_H),
            App::Program => (browser::PROGRAM_W, browser::PROGRAM_H),
        }
    }

    /// A name for settings files.
    fn key(self) -> &'static str {
        match self {
            App::Terminal => "terminal",
            App::Explorer => "explorer",
            App::Notepad => "notepad",
            App::Paint => "paint",
            App::Calculator => "calculator",
            App::Photos => "photos",
            App::Video => "video",
            App::Store => "store",
            App::Browser => "browser",
            App::Settings => "settings",
            App::About => "about",
            App::TaskManager => "taskmgr",
            App::Program => "program",
        }
    }

    fn from_key(key: &str) -> Option<App> {
        APPS.into_iter().find(|a| a.key() == key)
    }

    /// Other words search finds the app by, in English and Russian.
    fn keywords(self) -> &'static str {
        match self {
            App::Terminal => "cmd console shell command терминал консоль командная",
            App::Explorer => "files folders explorer computer проводник файлы папки компьютер",
            App::Notepad => "notepad text editor блокнот текст редактор",
            App::Paint => "paint draw picture рисование паинт рисовалка",
            App::Calculator => "calc калькулятор",
            App::Photos => "photo picture image viewer gallery фото фотографии просмотр картинки изображения галерея",
            App::Video => "video movie player avi видео фильм плеер кино",
            App::Store => "app store programs install games download магазин программы приложения установить игры скачать",
            App::Browser => "web internet browser браузер интернет",
            App::Settings => "control panel options параметры настройки",
            App::About => "about system winver о системе",
            App::TaskManager => "task manager processes performance cpu memory end task диспетчер задач процессы производительность память процессор снять задачу",
            App::Program => "",
        }
    }

    fn default_position(self) -> (i32, i32) {
        match self {
            App::Terminal => (240, 70),
            App::Explorer => (330, 110),
            App::Notepad => (520, 170),
            App::Paint => (520, 150),
            App::Calculator => (1440, 90),
            App::Photos => (300, 60),
            App::Video => (360, 90),
            App::Store => (400, 80),
            App::Browser => (200, 40),
            App::Settings => (420, 140),
            App::About => (680, 280),
            App::TaskManager => (560, 150),
            App::Program => (440, 110),
        }
    }

    /// Apps the launcher and search list: a program window only opens
    /// with a program in it.
    pub(super) fn listed(self) -> bool {
        self != App::Program
    }
}

/// Ask the desktop to open an app. Returns false in text mode.
pub fn request_open(app: App) -> bool {
    REQUESTS.push(app.index() as u8);
    ACTIVE.load(Ordering::Relaxed)
}

/// Ask the browser to go to an address (the shell's `browser` command).
pub fn request_address(address: &str) {
    browser::request_address(address);
}

/// A file for Notepad and a folder for File Explorer, from the shell.
static FILE_REQUEST: IrqMutex<Option<String>> = IrqMutex::new(None);
static FOLDER_REQUEST: IrqMutex<Option<String>> = IrqMutex::new(None);

/// Ask Notepad to open a file (the shell's `notepad` command).
pub fn request_file(path: &str) {
    *FILE_REQUEST.lock() = Some(String::from(path));
}

/// Ask File Explorer to show a folder (the shell's `explorer` command).
pub fn request_folder(path: &str) {
    *FOLDER_REQUEST.lock() = Some(String::from(path));
}

/// Ask the desktop to show the lock screen.
pub fn request_lock() -> bool {
    REQUESTS.push(LOCK);
    ACTIVE.load(Ordering::Relaxed)
}

/// Ask the desktop to restart or shut down, with the animation.
pub fn request_power(restart: bool) -> bool {
    REQUESTS.push(if restart { RESTART } else { SHUT_DOWN });
    ACTIVE.load(Ordering::Relaxed)
}

/// Switch between the light and the dark look (the shell's `theme`).
pub fn set_theme(dark: bool) {
    personalize::update(|p| p.dark = dark);
}

/// Use a picture as the desktop background (the shell's `wallpaper`).
/// Returns false if it can't be read as a picture.
pub fn set_wallpaper(path: &str) -> bool {
    let ok = fs::read(path)
        .ok()
        .is_some_and(|data| picture::decode(&data).is_some());
    if ok {
        personalize::set_wallpaper(path);
    }
    ok
}

/// The next built-in background.
pub fn next_wallpaper() {
    personalize::update(|p| {
        p.background = match p.background {
            personalize::Background::Builtin(i) => {
                personalize::Background::Builtin((i + 1) % wallpaper::BUILTIN.len())
            }
            _ => personalize::Background::Builtin(0),
        }
    });
}

/// Ask the desktop to close an app's window.
pub fn request_close(app: App) -> bool {
    REQUESTS.push(CLOSE | app.index() as u8);
    ACTIVE.load(Ordering::Relaxed)
}

/// A mouse event for an app, in its client coordinates.
#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub kind: MouseKind,
}

#[derive(Clone, Copy)]
pub enum MouseKind {
    Down {
        right: bool,
    },
    /// Moved with a button held.
    Move,
    Up,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    /// Opening or closing: grow or shrink a little and fade.
    Zoom,
    /// Flying to or from the taskbar button.
    Minimize,
}

#[derive(Clone, Copy)]
struct WindowAnim {
    motion: Motion,
    /// 0 is gone, ONE is fully there.
    tween: Tween,
}

#[derive(Clone, Copy)]
struct Window {
    /// Outer frame, including the title bar and border.
    rect: Rect,
    open: bool,
    minimized: bool,
    anim: Option<WindowAnim>,
    /// The virtual desktop it is on, and whether that is not the one
    /// shown now.
    desk: usize,
    away: bool,
    /// When it opened, for the order of taskbar buttons.
    opened: u64,
}

impl Window {
    fn visible(&self) -> bool {
        self.open && !self.minimized && !self.away
    }

    /// Whether it is on the screen, also while it animates away.
    fn drawn(&self) -> bool {
        self.visible() || self.anim.is_some()
    }

    fn client(&self) -> Rect {
        Rect::new(
            self.rect.x + BORDER,
            self.rect.y + TITLE_H,
            self.rect.w - 2 * BORDER,
            self.rect.h - TITLE_H - BORDER,
        )
    }

    fn title_bar(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLE_H)
    }

    /// The round caption buttons at the left of the title bar, with a
    /// little room around the dots for the mouse.
    fn close_button(&self) -> Rect {
        caption_button(self.rect, 0)
    }

    fn minimize_button(&self) -> Rect {
        caption_button(self.rect, 1)
    }

    /// Everything the window draws on, shadow included.
    fn bounds(&self) -> Rect {
        shadow_bounds(self.rect)
    }
}

/// A frame and its shadow.
fn shadow_bounds(r: Rect) -> Rect {
    Rect::new(
        r.x - SPREAD,
        r.y - SPREAD,
        r.w + 2 * SPREAD,
        r.h + 2 * SPREAD + SHADOW_DROP,
    )
}

/// What the screen shows.
#[derive(Clone, Copy)]
enum Phase {
    /// The boot screen, since this tick.
    Boot(u64),
    /// Restarting or shutting down, since this tick.
    Power(power::Power, u64),
    /// The lock screen or the sign-in panel.
    Login,
    /// The sign-in screen (in SNAPSHOT) fading into the desktop.
    Unlocking(Tween),
    /// The desktop (in SNAPSHOT) fading into the lock screen.
    Locking(Tween),
    Desktop,
}

/// What is under the mouse and lights up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    Minimize(App),
    Close(App),
    /// A button in the dock.
    Task(TaskItem),
    /// A menu bar button: the layout, quick settings, the clock, ^ or
    /// the logo.
    Tray(usize),
    ShowDesktop,
    Quick(tray::Target),
    /// An icon behind ^.
    Hidden(usize),
}

pub struct Desktop<'a> {
    fb: Framebuffer,
    back: &'static mut [u32],
    wallpaper: &'static mut [u32],
    width: i32,
    height: i32,
    boot: &'a BootInfo,
    icons: &'static Icons,
    pointer_image: Pointer,

    windows: [Window; APPS.len()],
    /// Open windows from bottom to top.
    order: [App; APPS.len()],
    order_len: usize,
    focused: Option<App>,

    mouse_x: i32,
    mouse_y: i32,
    left: bool,
    right: bool,
    /// Window being moved, and where in its title bar it was grabbed.
    drag: Option<(App, i32, i32)>,
    /// App that got the button press and gets the moves until release.
    capture: Option<App>,
    hover: Fader<Hover>,
    start: StartMenu,
    /// How far the start menu is open, for its slide.
    menu: Tween,
    menu_moving: bool,
    tray: Tray,
    /// The open tray flyout, and the one last shown for its slide.
    panel: Option<Panel>,
    panel_shown: Panel,
    panel_anim: Tween,
    panel_moving: bool,
    /// The quick settings slider being dragged.
    slider: Option<tray::Slider>,
    /// Asks the main loop to switch the keyboard layout.
    toggle_layout: bool,
    /// Copy the whole back buffer to the screen next frame (brightness).
    present_all: bool,
    phase: Phase,
    login: Login,
    /// Who the open windows belong to.
    session_user: Option<usize>,
    /// Mouse buttons as the PS/2 mouse and the vmmouse last reported them.
    ps2_buttons: (bool, bool),
    vm_buttons: (bool, bool),
    /// Last vmmouse position, to tell moves from button-only events.
    vm_position: (u32, u32),
    /// The absolute vmmouse is on (in QEMU and VMware).
    absolute: bool,

    layout: Layout,
    clock: StackString<16>,
    date: StackString<16>,
    cursor_on: bool,
    dirty: Dirty,
    snapshot: &'static mut [u32],
    scratch: &'static mut [u32],
    surfaces: [&'static mut [u32]; APPS.len()],
    /// Apps whose surface must be drawn again.
    stale: [bool; APPS.len()],

    terminal: terminal::Terminal,
    paint: paint::Paint,
    calc: calc::Calc,
    browser: Box<browser::Browser>,
    notepad: Box<notepad::Notepad>,
    explorer: Box<explorer::Explorer>,
    photos: Box<photos::Photos>,
    store: Box<store::Store>,
    taskmgr: Box<taskmgr::TaskManager>,
    /// The window a program runs in.
    program: Box<browser::Browser>,
    /// Its title when last drawn, to notice when the title bar changes.
    program_title: String,
    video: Box<video::Video>,
    settings: settings::Settings,
    about: about::About,
    /// The window the mouse was last over, for hover highlights.
    hover_app: Option<App>,

    /// Apps pinned to the taskbar, in order.
    pins: Vec<App>,
    /// What the mouse rests on, since when, and its tooltip.
    hover_now: Option<Hover>,
    hover_since: u64,
    tip_checked: bool,
    tip: Option<(Rect, String)>,
    /// Windows "Show desktop" minimised, to bring back.
    peeked: Vec<App>,
    popup: Option<Popup>,
    search: Box<Search>,
    /// Asking before emptying the Recycle Bin.
    confirm_empty: bool,
    desk_count: usize,
    current_desk: usize,
    /// Sliding to another desktop: progress and direction.
    slide: Option<(Tween, i32)>,
    /// Fading from SNAPSHOT to the new screen (Task View opening).
    crossfade: Option<Tween>,
    tv: TaskView,
    switcher: Option<Switcher>,
    /// The wallpaper shrunk, for desktop pictures in Task View.
    wall_thumb: Vec<u32>,
    desk_icons: DeskIcons,
    open_count: u64,
}

impl<'a> Desktop<'a> {
    pub fn new(fb: Framebuffer, boot: &'a BootInfo) -> Self {
        let width = fb.width.min(MAX_W) as i32;
        let height = fb.height.min(MAX_H) as i32;
        let mut windows = [Window {
            rect: Rect::default(),
            open: false,
            minimized: false,
            anim: None,
            desk: 0,
            away: false,
            opened: 0,
        }; APPS.len()];
        for app in APPS {
            let (w, h) = app.client_size();
            let (x, y) = app.default_position();
            // keep windows on small screens
            let x = x.min(width - w - 2 * BORDER).max(0);
            let y = y
                .min(height - TASKBAR_H - h - TITLE_H - BORDER)
                .max(MENUBAR_H);
            windows[app.index()].rect = Rect::new(x, y, w + 2 * BORDER, h + TITLE_H + BORDER);
        }
        let mut pool = SURFACES.take();
        let surfaces = core::array::from_fn(|i| {
            let (w, h) = APPS[i].client_size();
            let (mine, rest) = core::mem::take(&mut pool).split_at_mut((w * h) as usize);
            pool = rest;
            mine
        });
        let wallpaper = WALLPAPER.take();
        // the look the lock screen had before the restart
        personalize::load_boot();
        wallpaper::render(&personalize::get(), wallpaper, width, height);
        let login = Login::new(wallpaper, BACKDROP.take(), width, height);
        let mut wall_thumb = alloc::vec![0u32; (desktops::TILE_W * desktops::TILE_H) as usize];
        shrink_wallpaper(&mut wall_thumb, wallpaper, width, height);
        Self {
            fb,
            back: BACK_BUFFER.take(),
            wallpaper,
            width,
            height,
            boot,
            icons: icons::get(),
            pointer_image: Pointer::new(),
            windows,
            order: APPS,
            order_len: 0,
            focused: None,
            mouse_x: width / 2,
            mouse_y: height / 2,
            left: false,
            right: false,
            drag: None,
            capture: None,
            hover: Fader::new(anim::ms(120)),
            start: StartMenu::new(),
            menu: Tween::new(0, 0, 1),
            menu_moving: false,
            tray: Tray::new(),
            panel: None,
            panel_shown: Panel::Quick,
            panel_anim: Tween::new(0, 0, 1),
            panel_moving: false,
            slider: None,
            toggle_layout: false,
            present_all: false,
            phase: Phase::Boot(interrupts::ticks()),
            login,
            session_user: None,
            ps2_buttons: (false, false),
            vm_buttons: (false, false),
            vm_position: (0, 0),
            absolute: false,
            layout: Layout::Us,
            clock: StackString::new(),
            date: StackString::new(),
            cursor_on: true,
            dirty: Dirty::default(),
            snapshot: SNAPSHOT.take(),
            scratch: SCRATCH.take(),
            surfaces,
            stale: [true; APPS.len()],
            terminal: terminal::Terminal::new(),
            paint: paint::Paint::new(),
            calc: calc::Calc::new(),
            browser: Box::new(browser::Browser::new()),
            notepad: Box::new(notepad::Notepad::new()),
            explorer: Box::new(explorer::Explorer::new()),
            photos: Box::new(photos::Photos::new()),
            store: Box::new(store::Store::new()),
            taskmgr: Box::new(taskmgr::TaskManager::new()),
            program: Box::new(browser::Browser::program()),
            program_title: String::new(),
            video: Box::new(video::Video::new()),
            settings: settings::Settings::new(),
            about: about::About::new(),
            hover_app: None,
            pins: taskbar::DEFAULT_PINS.to_vec(),
            hover_now: None,
            hover_since: 0,
            tip_checked: true,
            tip: None,
            peeked: Vec::new(),
            popup: None,
            search: Box::new(Search::new()),
            confirm_empty: false,
            desk_count: 1,
            current_desk: 0,
            slide: None,
            crossfade: None,
            tv: TaskView::new(),
            switcher: None,
            wall_thumb,
            desk_icons: DeskIcons::new(),
            open_count: 0,
        }
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    fn damage(&mut self, r: Rect) {
        self.dirty.add(r.intersect(&self.screen()));
    }

    fn damage_window(&mut self, app: App) {
        let w = self.windows[app.index()];
        if w.visible() {
            self.damage(w.bounds());
        }
    }

    // ---- animations -------------------------------------------------------

    /// Start a window moving towards `to` (0 gone, ONE there). An
    /// animation already running the same way turns around smoothly.
    fn animate(&mut self, app: App, motion: Motion, to: i32) {
        let duration = match (motion, to) {
            (Motion::Zoom, ONE) => anim::ms(200),
            (Motion::Zoom, _) => anim::ms(150),
            (Motion::Minimize, _) => anim::ms(260),
        };
        let w = &mut self.windows[app.index()];
        match &mut w.anim {
            Some(a) if a.motion == motion => a.tween.retarget(to, duration),
            _ => {
                let tween = Tween::new(ONE - to, to, duration);
                w.anim = Some(WindowAnim { motion, tween });
            }
        }
        self.damage(self.anim_envelope(app));
    }

    /// Where a window minimises to: a small frame over its taskbar button.
    fn minimized_rect(&self, app: App) -> Rect {
        let r = self.windows[app.index()].rect;
        let slot = self.task_rect(TaskItem::App(app)).unwrap_or(Rect::new(
            self.width / 2 - 22,
            self.height - TASKBAR_H,
            44,
            40,
        ));
        let (w, h) = (r.w / 6, r.h / 6);
        Rect::new(
            slot.x + slot.w / 2 - w / 2,
            self.height - TASKBAR_H - h / 2,
            w,
            h,
        )
    }

    /// The frame of an animating window now, and how opaque it is.
    fn anim_frame(&self, app: App) -> Option<(Rect, i32)> {
        let w = &self.windows[app.index()];
        let a = w.anim?;
        let p = a.tween.value();
        let r = w.rect;
        Some(match a.motion {
            Motion::Zoom => {
                let scale = lerp(ONE * 92 / 100, ONE, p);
                let (nw, nh) = (r.w * scale / ONE, r.h * scale / ONE);
                let frame = Rect::new(r.x + (r.w - nw) / 2, r.y + (r.h - nh) / 2, nw, nh);
                (frame, p)
            }
            Motion::Minimize => {
                let t = self.minimized_rect(app);
                let frame = Rect::new(
                    lerp(t.x, r.x, p),
                    lerp(t.y, r.y, p),
                    lerp(t.w, r.w, p),
                    lerp(t.h, r.h, p),
                );
                // stay solid for most of the way
                (frame, (p * 2).min(ONE))
            }
        })
    }

    /// Everything an animating window may cover on its way.
    fn anim_envelope(&self, app: App) -> Rect {
        let w = &self.windows[app.index()];
        match w.anim {
            Some(a) if a.motion == Motion::Minimize => {
                w.bounds().union(&shadow_bounds(self.minimized_rect(app)))
            }
            _ => w.bounds(),
        }
    }

    /// Move every animation on by the time that has passed and mark
    /// what it changes.
    fn tick(&mut self) {
        self.tick_power();
        match self.phase {
            Phase::Unlocking(t) | Phase::Locking(t) if t.done() => {
                self.phase = match self.phase {
                    Phase::Locking(_) => Phase::Login,
                    _ => Phase::Desktop,
                };
                self.damage(self.screen());
            }
            _ => {}
        }
        self.login.tick();
        let (rects, n) = self.login.dirty.take();
        if matches!(self.phase, Phase::Login | Phase::Locking(_)) {
            for r in &rects[..n] {
                self.damage(*r);
            }
        }

        for app in APPS {
            let Some(a) = self.windows[app.index()].anim else {
                continue;
            };
            self.damage(self.anim_envelope(app));
            if a.tween.done() {
                let w = &mut self.windows[app.index()];
                w.anim = None;
                if a.motion == Motion::Zoom && !w.open {
                    self.remove_from_order(app);
                }
            }
        }
        // one more frame once it stops, to draw where it ended
        let menu_moving = !self.menu.done();
        let menu_was_moving = core::mem::replace(&mut self.menu_moving, menu_moving);
        if menu_moving || menu_was_moving || self.start.tick() {
            let r = self.menu_rect();
            self.damage(r.union(&r.offset(0, 48)));
        }
        let panel_moving = !self.panel_anim.done();
        let panel_was_moving = core::mem::replace(&mut self.panel_moving, panel_moving);
        if panel_moving || panel_was_moving {
            let r = self.panel_rect(self.panel_shown).inset(-SPREAD);
            self.damage(r.union(&r.offset(0, 48)));
        }
        if self.hover.tick() {
            for h in self.hover.lit().into_iter().flatten() {
                self.damage(self.hover_rect(h));
            }
        }
        if self.slide.is_some_and(|(t, _)| t.done()) {
            self.slide = None;
            self.damage(self.screen());
        }
        if self.crossfade.is_some_and(|t| t.done()) {
            self.crossfade = None;
            self.damage(self.screen());
        }
        self.tick_tip();
    }

    // ---- personalization -------------------------------------------------

    /// The colours or the background changed (Settings, File Explorer or
    /// Paint): draw everything again, the background too when
    /// `background` is set, fading from the old look on the desktop.
    fn apply_look(&mut self, background: bool) {
        if matches!(self.phase, Phase::Desktop) && self.slide.is_none() {
            self.start_crossfade();
            if let Some(t) = &mut self.crossfade {
                *t = Tween::new(0, ONE, anim::ms(350));
            }
        }
        if background {
            let prefs = personalize::get();
            if !wallpaper::render(&prefs, self.wallpaper, self.width, self.height) {
                serial::write_str("wallpaper: could not read the picture\n");
            }
            self.login.set_wallpaper(self.wallpaper);
            shrink_wallpaper(
                &mut self.wall_thumb,
                self.wallpaper,
                self.width,
                self.height,
            );
            serial::write_str("wallpaper: changed\n");
        }
        serial::write_str(if theme::dark() {
            "look: dark\n"
        } else {
            "look: light\n"
        });
        self.stale = [true; APPS.len()];
        self.damage(self.screen());
    }

    // ---- signing in and out -----------------------------------------------

    /// Keep what the screen shows now, without the pointer, to fade from.
    fn take_snapshot(&mut self) {
        let mut back = core::mem::take(&mut self.back);
        let mut scratch = core::mem::take(&mut self.scratch);
        {
            let mut c = Canvas::new(back, self.width as usize, self.height as usize);
            self.draw_scene(&mut c, scratch);
        }
        let n = (self.width * self.height) as usize;
        self.snapshot[..n].copy_from_slice(&back[..n]);
        core::mem::swap(&mut self.back, &mut back);
        core::mem::swap(&mut self.scratch, &mut scratch);
    }

    fn signed_in(&mut self) {
        self.take_snapshot();
        let user = users::current();
        if let Some(name) = users::current_name() {
            fs::ensure_home(name.as_str());
        }
        // the user's own colours and background, before the desktop shows
        personalize::load_user();
        if let Some(background) = personalize::take_changed() {
            self.apply_look(background);
        }
        if self.session_user != user {
            // someone else: start a fresh session
            self.close_all();
            self.session_user = user;
            *self.notepad = notepad::Notepad::new();
            *self.explorer = explorer::Explorer::new();
            self.load_pins();
            self.search.forget();
            self.desk_icons = DeskIcons::new();
            self.refresh_icons();
        }
        self.phase = Phase::Unlocking(Tween::new(0, ONE, anim::ms(450)));
        self.damage(self.screen());
        if self.order_len == 0 {
            self.open(App::Terminal);
        }
    }

    fn lock(&mut self, sign_out: bool) {
        if !matches!(self.phase, Phase::Desktop) {
            return;
        }
        self.start.open = false;
        self.menu = Tween::new(0, 0, 1);
        self.panel = None;
        self.panel_anim = Tween::new(0, 0, 1);
        self.slider = None;
        self.drag = None;
        self.capture = None;
        self.popup = None;
        self.tip = None;
        self.search.open = false;
        self.switcher = None;
        self.confirm_empty = false;
        self.tv.open = false;
        self.slide = None;
        self.crossfade = None;
        self.take_snapshot();
        if sign_out {
            users::sign_out();
            self.close_all();
            self.session_user = None;
        }
        self.login.lock();
        self.phase = Phase::Locking(Tween::new(0, ONE, anim::ms(450)));
        self.damage(self.screen());
    }

    /// Close every window at once, without animations.
    fn close_all(&mut self) {
        for w in &mut self.windows {
            w.open = false;
            w.minimized = false;
            w.anim = None;
            w.desk = 0;
            w.away = false;
        }
        self.order_len = 0;
        self.focused = None;
        self.desk_count = 1;
        self.current_desk = 0;
        self.peeked.clear();
    }

    fn login_outcome(&mut self, outcome: login::Outcome) {
        match outcome {
            login::Outcome::None => {}
            login::Outcome::SignedIn => self.signed_in(),
            login::Outcome::Restart => self.power(power::Power::Restart),
            login::Outcome::ShutDown => self.power(power::Power::ShutDown),
        }
    }

    /// The app's content changed: draw its surface again.
    fn damage_client(&mut self, app: App) {
        self.stale[app.index()] = true;
        let w = self.windows[app.index()];
        if w.visible() {
            self.damage(w.client());
        }
    }

    /// An app changed. Notepad and File Explorer show the file or folder
    /// in the title bar, so their whole window is drawn again.
    fn app_changed(&mut self, app: App) {
        match app {
            App::Notepad | App::Explorer | App::Photos | App::Video | App::Program => {
                self.stale[app.index()] = true;
                self.damage_window(app);
            }
            _ => self.damage_client(app),
        }
    }

    /// The open windows, for Task Manager.
    fn task_rows(&self) -> Vec<taskmgr::Row> {
        APPS.into_iter()
            .filter(|&a| self.windows[a.index()].open)
            .map(|a| {
                let w = self.windows[a.index()];
                let (cw, ch) = a.client_size();
                taskmgr::Row {
                    app: a,
                    name: self.window_title(a),
                    state: if w.minimized {
                        "Minimized"
                    } else if w.away {
                        "On another desktop"
                    } else {
                        "Running"
                    },
                    memory: cw as u64 * ch as u64 * 4,
                }
            })
            .collect()
    }

    /// The text in a window's title bar.
    fn window_title(&self, app: App) -> String {
        match app {
            App::Notepad => self.notepad.title(),
            App::Explorer => self.explorer.title(),
            App::Photos => self.photos.title(),
            App::Video => self.video.title(),
            App::Program => self.program.program_title(),
            _ => String::from(app.title()),
        }
    }

    /// Act on what Notepad and File Explorer ask for, and on files and
    /// folders the shell asked for.
    fn poll_apps(&mut self) {
        if core::mem::take(&mut self.notepad.want_close) {
            self.close(App::Notepad);
        }
        if self.windows[App::Explorer.index()].open && self.explorer.check_changes() {
            self.app_changed(App::Explorer);
        }
        if let Some(path) = self.explorer.open_request.take() {
            self.open_file(&path);
        }
        while let Some(req) = browser::take_desktop_request() {
            if let Some(path) = req.strip_prefix("open:") {
                if fs::is_dir(path) {
                    self.show_folder(path);
                } else {
                    self.open_file(path);
                }
            } else if let Some(path) = req.strip_prefix("folder:") {
                self.show_folder(path);
            }
        }
        if let Some(path) = self.store.open_request.take() {
            self.open_file(&path);
        }
        if let Some(path) = self.photos.edit_request.take() {
            self.open(App::Paint);
            self.paint.open_file(&path);
            self.damage_client(App::Paint);
        }
        let file = FILE_REQUEST.lock().take();
        if let Some(path) = file {
            self.open_file(&path);
        }
        let folder = FOLDER_REQUEST.lock().take();
        if let Some(path) = folder {
            self.explorer.show(&path);
            self.open(App::Explorer);
            self.app_changed(App::Explorer);
        }
    }

    /// Show a folder in File Explorer.
    fn show_folder(&mut self, path: &str) {
        self.explorer.show(path);
        self.open(App::Explorer);
        self.app_changed(App::Explorer);
    }

    fn open_settings(&mut self, page: settings::Page) {
        self.settings.show_page(page);
        self.open(App::Settings);
        self.damage_client(App::Settings);
    }

    /// Open a file: pictures in Photos, videos in Video Player, web
    /// pages and downloaded programs in the browser, everything else in
    /// the text editor.
    fn open_file(&mut self, path: &str) {
        if picture::is_picture(path) {
            self.open(App::Photos);
            self.photos.open_file(path);
            self.app_changed(App::Photos);
            return;
        }
        if video::is_video(path) {
            self.open(App::Video);
            self.video.open_file(path);
            self.app_changed(App::Video);
            return;
        }
        if path.to_ascii_lowercase().ends_with(crate::web::LINK_EXT) {
            // a desktop shortcut: open what it points at
            match crate::web::link_target(path) {
                Some(target) if !target.to_ascii_lowercase().ends_with(crate::web::LINK_EXT) => {
                    if fs::exists(&target) {
                        self.open_file(&target);
                    } else {
                        serial::write_str("desktop: the shortcut's program is gone\n");
                    }
                }
                _ => {}
            }
            return;
        }
        if path.to_ascii_lowercase().ends_with(crate::web::PROGRAM_EXT) {
            self.open(App::Program);
            self.program.open_file(path);
            self.app_changed(App::Program);
            return;
        }
        if browser::is_page(path) {
            self.open(App::Browser);
            self.browser.open_file(path);
            self.damage_client(App::Browser);
            return;
        }
        self.open(App::Notepad);
        self.notepad.open_file(path);
        self.app_changed(App::Notepad);
    }

    // ---- window management ----------------------------------------------

    fn open(&mut self, app: App) {
        let w = self.windows[app.index()];
        if w.open && w.away {
            // it is on another desktop: go there
            self.switch_desktop(w.desk);
        }
        self.open_count += 1;
        let (count, desk) = (self.open_count, self.current_desk);
        let w = &mut self.windows[app.index()];
        if !w.open {
            w.open = true;
            w.minimized = false;
            w.desk = desk;
            w.away = false;
            w.opened = count;
            // for the boot test, which only sees the serial port
            serial::write_str("\ndesktop: opened ");
            serial::write_str(app.title());
            serial::write_str("\n");
            if app.listed() {
                self.start.note_opened(app);
            }
            self.animate(app, Motion::Zoom, ONE);
        } else if w.minimized {
            w.minimized = false;
            self.animate(app, Motion::Minimize, ONE);
        }
        if app == App::Browser {
            self.browser.start();
        }
        if app == App::Explorer {
            self.explorer.start();
            self.stale[app.index()] = true;
        }
        if app == App::Store {
            self.store.start();
        }
        self.focus(app);
        self.damage_taskbar();
    }

    fn close(&mut self, app: App) {
        if !self.windows[app.index()].open {
            return;
        }
        // Notepad first asks about unsaved changes
        if app == App::Notepad && !self.notepad.try_close() {
            self.focus(app);
            self.app_changed(app);
            return;
        }
        if app == App::Video {
            self.video.stop();
        }
        if app == App::Program {
            self.program.close_program();
        }
        self.damage_window(app);
        // it stays in the stacking order until it has faded out
        self.windows[app.index()].open = false;
        self.animate(app, Motion::Zoom, 0);
        if self.focused == Some(app) {
            self.focus_top();
        }
        self.damage_taskbar();
    }

    fn minimize(&mut self, app: App) {
        self.damage_window(app);
        self.windows[app.index()].minimized = true;
        self.animate(app, Motion::Minimize, 0);
        if self.focused == Some(app) {
            self.focus_top();
        }
        self.damage_taskbar();
    }

    fn remove_from_order(&mut self, app: App) {
        if let Some(i) = self.order[..self.order_len].iter().position(|&a| a == app) {
            self.order.copy_within(i + 1..self.order_len, i);
            self.order_len -= 1;
        }
    }

    /// Let the desktop have the keyboard: no window is active.
    fn focused_away(&mut self) {
        if let Some(old) = self.focused.take() {
            self.damage_window(old);
            self.damage_taskbar();
        }
    }

    /// Show a right-click menu.
    fn show_popup(&mut self, menu: Popup) {
        if let Some(old) = self.popup.take() {
            self.damage(old.bounds());
        }
        self.hover_moved(None);
        self.damage(menu.bounds());
        self.popup = Some(menu);
    }

    fn close_popup(&mut self) {
        if let Some(old) = self.popup.take() {
            self.damage(old.bounds());
        }
    }

    /// Do what a right-click menu item says.
    fn run_cmd(&mut self, cmd: Cmd) {
        self.close_popup();
        match cmd {
            Cmd::Open(app) => self.open(app),
            Cmd::Pin(app) => self.pin(app),
            Cmd::Unpin(app) => self.unpin(app),
            Cmd::Close(app) => {
                self.close(app);
                if self.tv.open {
                    self.damage(self.screen());
                }
            }
            Cmd::Minimize(app) => self.minimize(app),
            Cmd::MoveTo(app, d) => self.move_to_desktop(app, Some(d)),
            Cmd::MoveToNew(app) => self.move_to_desktop(app, None),
            Cmd::NewDesktop => {
                if let Some(d) = self.new_desktop() {
                    self.switch_desktop(d);
                }
            }
            Cmd::TaskView => self.toggle_task_view(),
            Cmd::ShowDesktop => self.toggle_show_desktop(),
            Cmd::Search => self.open_search(),
            Cmd::OpenIcon(i) => self.open_icon(i),
            Cmd::RenameIcon(i) => self.rename_icon(i),
            Cmd::DeleteIcons => self.delete_icons(),
            Cmd::EmptyBin => {
                self.confirm_empty = true;
                self.damage(self.screen());
            }
            Cmd::Refresh => {
                self.desk_icons.forget();
                self.refresh_icons();
            }
            Cmd::ArrangeIcons => self.arrange_icons(),
            Cmd::Personalize => self.open_settings(settings::Page::Personalization),
            Cmd::DisplaySettings => self.open_settings(settings::Page::System),
            Cmd::NextBackground => next_wallpaper(),
            Cmd::SetBackground(i) => {
                if let Some(path) = self.icon_path(i) {
                    personalize::set_wallpaper(&path);
                }
            }
            Cmd::NewFolder => self.new_on_desktop(true),
            Cmd::NewFile => self.new_on_desktop(false),
            Cmd::Lock => self.lock(false),
            Cmd::SignOut => self.lock(true),
            Cmd::Restart => self.power(power::Power::Restart),
            Cmd::ShutDown => self.power(power::Power::ShutDown),
        }
    }

    /// The Yes and No of "Empty the Recycle Bin?".
    fn confirm_rects(&self) -> (Rect, Vec<Rect>) {
        let area = Rect::new(0, 0, self.width, self.height - TASKBAR_H);
        let r = widgets::message_rect(area, &CONFIRM_LINES, &["Yes", "No"]);
        (r, widgets::message_buttons(r, 2))
    }

    fn answer_confirm(&mut self, yes: bool) {
        self.confirm_empty = false;
        self.damage(self.screen());
        if yes {
            self.empty_bin();
            self.app_changed(App::Explorer);
        }
    }

    /// Raise a window to the top and give it the keyboard.
    fn focus(&mut self, app: App) {
        if self.focused != Some(app) {
            if let Some(old) = self.focused {
                self.damage_window(old);
            }
            self.damage_taskbar();
        }
        self.remove_from_order(app);
        self.order[self.order_len] = app;
        self.order_len += 1;
        self.focused = Some(app);
        self.cursor_on = true;
        self.damage_window(app);
    }

    fn focus_top(&mut self) {
        self.focused = None;
        let top = self.order[..self.order_len]
            .iter()
            .rev()
            .find(|&&a| self.windows[a.index()].visible())
            .copied();
        if let Some(app) = top {
            self.focus(app);
        }
    }

    /// The topmost visible window under a point.
    fn window_at(&self, x: i32, y: i32) -> Option<App> {
        self.order[..self.order_len]
            .iter()
            .rev()
            .find(|a| {
                let w = &self.windows[a.index()];
                w.visible() && w.rect.contains(x, y)
            })
            .copied()
    }

    fn move_window(&mut self, app: App, x: i32, y: i32) {
        let w = self.windows[app.index()];
        // keep part of the title bar on the screen
        let x = x.clamp(80 - w.rect.w, self.width - 80);
        let y = y.clamp(MENUBAR_H, self.height - TASKBAR_H - TITLE_H);
        if (x, y) != (w.rect.x, w.rect.y) {
            self.damage(w.bounds());
            self.windows[app.index()].rect.x = x;
            self.windows[app.index()].rect.y = y;
            self.damage_window(app);
        }
    }

    // ---- input -------------------------------------------------------------

    fn on_key(&mut self, key: Key) {
        match self.phase {
            Phase::Login => {
                let outcome = self.login.on_key(key);
                self.login_outcome(outcome);
                return;
            }
            Phase::Locking(_) | Phase::Boot(_) | Phase::Power(..) => return,
            // typing can start while the desktop fades in
            Phase::Unlocking(_) | Phase::Desktop => {}
        }
        if let Key::LayoutChanged = key {
            self.damage_taskbar();
            self.damage_panel();
            self.damage_client(App::Settings);
            return;
        }
        if self.confirm_empty {
            match key {
                Key::Enter => self.answer_confirm(true),
                Key::Escape => self.answer_confirm(false),
                _ => {}
            }
            return;
        }
        if self.popup.is_some() {
            self.close_popup();
            if let Key::Escape = key {
                return;
            }
        }
        // Alt+Tab and Alt+F4
        if let Key::AltUp = key {
            self.alt_up();
            return;
        }
        if keyboard::alt_held() {
            match key {
                Key::Char('\t') => return self.alt_tab(),
                Key::Function(4) => {
                    if let Some(app) = self.focused {
                        self.close(app);
                    }
                    return;
                }
                _ => {}
            }
        }
        if self.switcher.is_some() {
            if let Key::Escape = key {
                self.cancel_switcher();
            }
            return;
        }
        if matches!(key, Key::Escape) && keyboard::ctrl_held() && keyboard::shift_held() {
            self.open(App::TaskManager);
            return;
        }
        if keyboard::super_held() {
            self.win_shortcut(key);
            return;
        }
        if let Key::Super = key {
            if self.start.open {
                self.close_menu();
            } else {
                self.open_menu();
            }
            return;
        }
        if self.tv.open {
            self.tv_key(key);
            return;
        }
        if self.panel.is_some() {
            if let Key::Escape = key {
                self.close_panel();
                return;
            }
        }
        if self.search.open {
            let action = self.search.on_key(key);
            self.search_action(action);
            return;
        }
        if self.start.open {
            match self.start.on_key(key) {
                start::Action::None => {}
                action => self.menu_action(action),
            }
            return;
        }
        let Some(app) = self.focused else {
            if matches!(self.phase, Phase::Desktop) {
                self.icons_key(key);
            }
            return;
        };
        let changed = match app {
            App::Terminal => {
                self.terminal.on_key(key, self.boot);
                self.cursor_on = true;
                false // the console reports its own changes
            }
            App::Calculator => self.calc.on_key(key),
            App::Browser => self.browser.on_key(key),
            App::Notepad => self.notepad.on_key(key),
            App::Explorer => self.explorer.on_key(key),
            App::Settings => self.settings.on_key(key),
            App::Paint => self.paint.on_key(key),
            App::Photos => self.photos.on_key(key),
            App::Video => self.video.on_key(key),
            App::Store => self.store.on_key(key),
            App::Program => self.program.on_key(key),
            App::About | App::TaskManager => false,
        };
        if changed {
            self.cursor_on = true;
            self.app_changed(app);
        }
    }

    /// Win plus a key.
    fn win_shortcut(&mut self, key: Key) {
        let ctrl = keyboard::ctrl_held();
        match key {
            Key::Char('\t') => self.toggle_task_view(),
            Key::Left if ctrl => {
                let d = self.current_desk;
                if d > 0 {
                    self.switch_desktop(d - 1);
                }
            }
            Key::Right if ctrl => self.switch_desktop(self.current_desk + 1),
            Key::Ctrl('d') => {
                if let Some(d) = self.new_desktop() {
                    self.switch_desktop(d);
                }
            }
            Key::Function(4) if ctrl => {
                let d = self.current_desk;
                self.close_desktop(d);
            }
            Key::Char(c) => match c.to_ascii_lowercase() {
                'd' | 'в' | 'm' | 'ь' => self.toggle_show_desktop(),
                'e' | 'у' => {
                    self.explorer.start();
                    self.open(App::Explorer);
                }
                's' | 'ы' | 'q' | 'й' => self.open_search(),
                'l' | 'д' => self.lock(false),
                'i' | 'ш' => self.open(App::Settings),
                'x' | 'ч' => {
                    let r = self.task_rect(TaskItem::Start).unwrap_or_default();
                    let menu = self.start_menu_popup(r.x, self.height - TASKBAR_H - 8);
                    self.show_popup(menu);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// A PS/2 mouse packet: relative movement.
    fn on_ps2(&mut self, packet: ps2::MousePacket) {
        self.ps2_buttons = (packet.left, packet.right);
        self.pointer(self.mouse_x + packet.dx, self.mouse_y + packet.dy);
        // with the absolute vmmouse the same wheel turn also comes from it
        if !self.absolute {
            self.wheel(packet.wheel);
        }
    }

    /// A vmmouse event: an absolute position.
    fn on_vmmouse(&mut self, ev: vmmouse::Event) {
        self.vm_buttons = (
            ev.buttons & vmmouse::LEFT != 0,
            ev.buttons & vmmouse::RIGHT != 0,
        );
        // button-only events repeat the last position; skip those, so the
        // PS/2 mouse (as QEMU's monitor drives it) is not thrown back
        let (mut x, mut y) = (self.mouse_x, self.mouse_y);
        if (ev.x, ev.y) != self.vm_position {
            self.vm_position = (ev.x, ev.y);
            x = (ev.x as u64 * self.width as u64 / 65536) as i32;
            y = (ev.y as u64 * self.height as u64 / 65536) as i32;
        }
        self.pointer(x, y);
        self.wheel(ev.wheel);
    }

    /// Scroll the window under the mouse.
    fn wheel(&mut self, clicks: i32) {
        if clicks != 0 && matches!(self.phase, Phase::Desktop) {
            let app = self.window_at(self.mouse_x, self.mouse_y);
            let changed = match app {
                Some(App::Browser) => self.browser.on_wheel(clicks),
                Some(App::Notepad) => self.notepad.on_wheel(clicks),
                Some(App::Explorer) => self.explorer.on_wheel(clicks),
                Some(App::Settings) => self.settings.on_wheel(clicks),
                Some(App::Paint) => self.paint.on_wheel(clicks),
                Some(App::Photos) => self.photos.on_wheel(clicks),
                Some(App::Video) => self.video.on_wheel(clicks),
                Some(App::Store) => self.store.on_wheel(clicks),
                Some(App::Program) => self.program.on_wheel(clicks),
                _ => false,
            };
            if let Some(app) = app.filter(|_| changed) {
                self.app_changed(app);
            }
        }
    }

    /// Move the pointer to a position and handle button changes.
    fn pointer(&mut self, x: i32, y: i32) {
        let (old_x, old_y) = (self.mouse_x, self.mouse_y);
        self.mouse_x = x.clamp(0, self.width - 1);
        self.mouse_y = y.clamp(0, self.height - 1);
        let moved = (old_x, old_y) != (self.mouse_x, self.mouse_y);
        if moved {
            self.damage(pointer_rect(old_x, old_y));
            self.damage(pointer_rect(self.mouse_x, self.mouse_y));
        }

        let (was_left, was_right) = (self.left, self.right);
        self.left = self.ps2_buttons.0 || self.vm_buttons.0;
        self.right = self.ps2_buttons.1 || self.vm_buttons.1;

        match self.phase {
            Phase::Desktop => {}
            Phase::Login => {
                self.login.on_move(self.mouse_x, self.mouse_y);
                if self.left && !was_left {
                    let outcome = self.login.on_click(self.mouse_x, self.mouse_y);
                    self.login_outcome(outcome);
                }
                return;
            }
            Phase::Unlocking(_) | Phase::Locking(_) | Phase::Boot(_) | Phase::Power(..) => return,
        }

        if self.left && !was_left {
            self.press(false);
        } else if self.right && !was_right {
            self.press(true);
        } else if moved && (self.left || self.right) {
            self.held_move();
        } else if moved {
            self.hover_apps();
            if let Some(p) = &mut self.popup {
                if p.set_hover(self.mouse_x, self.mouse_y) {
                    let r = p.rect;
                    self.damage(r);
                }
            }
            if self.tv.open {
                self.tv_hover(self.mouse_x, self.mouse_y);
            }
            if self.search.open {
                let (p, top) = (self.search_panel(), self.top_apps());
                if self.search.set_hover(p, &top, self.mouse_x, self.mouse_y) {
                    self.damage(p);
                }
            }
            let over_desktop = !self.tv.open
                && self.popup.is_none()
                && self.mouse_y < self.height - TASKBAR_H
                && self.mouse_y >= MENUBAR_H
                && self.window_at(self.mouse_x, self.mouse_y).is_none();
            self.icons_hover(self.mouse_x, self.mouse_y, over_desktop);
        }
        if (was_left && !self.left) || (was_right && !self.right) {
            self.release();
        }
        self.update_hover();
    }

    /// Tell the app under the mouse where it is, so it can light up what
    /// is under it (the browser shows where a link goes).
    fn hover_apps(&mut self) {
        let app = self
            .window_at(self.mouse_x, self.mouse_y)
            .filter(|_| !self.tv.open);
        // the window the mouse left forgets its highlight
        if let Some(old) = self.hover_app.filter(|&a| Some(a) != app) {
            if self.app_hover(old, -1000, -1000) {
                self.app_changed(old);
            }
        }
        self.hover_app = app;
        if let Some(app) = app {
            let client = self.windows[app.index()].client();
            if self.app_hover(app, self.mouse_x - client.x, self.mouse_y - client.y) {
                self.app_changed(app);
            }
        }
    }

    fn app_hover(&mut self, app: App, x: i32, y: i32) -> bool {
        match app {
            App::Browser => self.browser.on_hover(x, y),
            App::Notepad => self.notepad.on_hover(x, y),
            App::Explorer => self.explorer.on_hover(x, y),
            App::Video => self.video.on_hover(x, y),
            App::Store => self.store.on_hover(x, y),
            App::Program => self.program.on_hover(x, y),
            _ => false,
        }
    }

    /// Light up whatever is under the mouse now.
    fn update_hover(&mut self) {
        let hover = if self.drag.is_some() || self.desk_icons.busy() {
            None
        } else {
            self.hover_at(self.mouse_x, self.mouse_y)
        };
        self.hover_moved(hover);
        if self.hover.set(hover) {
            for h in self.hover.lit().into_iter().flatten() {
                self.damage(self.hover_rect(h));
            }
        }
        if self.start.open
            && self
                .start
                .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y)
        {
            self.damage(self.menu_rect());
        }
    }

    fn hover_at(&self, x: i32, y: i32) -> Option<Hover> {
        if self.popup.as_ref().is_some_and(|p| p.rect.contains(x, y)) || self.confirm_empty {
            return None;
        }
        if self.start.open && self.menu_panel().contains(x, y) {
            return None;
        }
        if self.search.open && self.search_panel().contains(x, y) {
            return None;
        }
        if let Some(panel) = self.panel {
            let r = self.panel_rect(panel);
            if r.contains(x, y) {
                return match panel {
                    Panel::Quick => self.tray.target_at(r, x, y).map(Hover::Quick),
                    Panel::Calendar => None,
                    Panel::Hidden => (0..tray::HIDDEN_ICONS)
                        .find(|&i| tray::hidden_icon_rect(r, i).contains(x, y))
                        .map(|i| Hover::Hidden(i as usize)),
                };
            }
        }
        if y < MENUBAR_H {
            return (0..taskbar::TRAY_BUTTONS)
                .find(|&i| self.tray_rect(i).contains(x, y))
                .map(Hover::Tray);
        }
        if y >= self.height - TASKBAR_H {
            if self.show_desktop_rect().contains(x, y) {
                return Some(Hover::ShowDesktop);
            }
            return self.task_at(x, y).map(Hover::Task);
        }
        if self.tv.open {
            return None;
        }
        let app = self.window_at(x, y)?;
        let w = self.windows[app.index()];
        if w.close_button().contains(x, y) {
            Some(Hover::Close(app))
        } else if w.minimize_button().contains(x, y) {
            Some(Hover::Minimize(app))
        } else {
            None
        }
    }

    fn hover_rect(&self, hover: Hover) -> Rect {
        match hover {
            Hover::Minimize(app) => self.windows[app.index()].minimize_button(),
            Hover::Close(app) => self.windows[app.index()].close_button(),
            Hover::Task(item) => self.task_rect(item).unwrap_or_default(),
            Hover::Tray(i) => self.tray_rect(i),
            Hover::ShowDesktop => self.show_desktop_rect().inset(-2),
            Hover::Quick(_) => self.panel_rect(Panel::Quick),
            Hover::Hidden(_) => self.panel_rect(Panel::Hidden),
        }
    }

    fn press(&mut self, right: bool) {
        let (x, y) = (self.mouse_x, self.mouse_y);
        self.hover_moved(None);
        if self.confirm_empty {
            if !right {
                let (_, buttons) = self.confirm_rects();
                if let Some(i) = buttons.iter().position(|b| b.contains(x, y)) {
                    self.answer_confirm(i == 0);
                }
            }
            return;
        }
        if let Some(p) = &self.popup {
            let inside = p.rect.contains(x, y);
            let cmd = p.cmd_at(x, y);
            if inside {
                if let (Some(cmd), false) = (cmd, right) {
                    self.run_cmd(cmd);
                }
                return;
            }
            self.close_popup();
            if !right {
                return;
            }
        }
        if self.switcher.is_some() {
            return;
        }
        if self.search.open {
            let panel = self.search_panel();
            if panel.contains(x, y) {
                if !right {
                    let (top, pins) = (self.top_apps(), self.pins.clone());
                    let action = self.search.on_click(panel, &top, &pins, x, y);
                    self.search_action(action);
                }
                return;
            }
            let on_box = self
                .task_rect(TaskItem::Search)
                .is_some_and(|r| r.contains(x, y));
            self.close_search();
            if on_box && !right {
                return;
            }
        }
        if self.start.open {
            let panel = self.menu_panel();
            if panel.contains(x, y) {
                if !right {
                    let action = self.start.on_click(panel, x, y);
                    self.menu_action(action);
                }
                return;
            }
            let on_start = self
                .task_rect(TaskItem::Start)
                .is_some_and(|r| r.contains(x, y));
            self.close_menu();
            if on_start && !right {
                return;
            }
        }
        if let Some(panel) = self.panel {
            let r = self.panel_rect(panel);
            if r.contains(x, y) {
                if !right && panel == Panel::Quick {
                    self.quick_click(r, x, y);
                }
                if !right && panel == Panel::Hidden {
                    if let Some(i) = (0..tray::HIDDEN_ICONS)
                        .find(|&i| tray::hidden_icon_rect(r, i).contains(x, y))
                    {
                        self.hidden_click(i as usize);
                    }
                }
                return;
            }
            // a click on the button that opened it only closes it
            let own = self.tray_rect(match panel {
                Panel::Quick => 1,
                Panel::Calendar => 2,
                Panel::Hidden => 3,
            });
            self.close_panel();
            if own.contains(x, y) {
                return;
            }
        }
        if y < MENUBAR_H {
            if self.tv.open {
                self.close_task_view(false);
            }
            self.taskbar_press(x, y, right);
            return;
        }
        if y >= self.height - TASKBAR_H {
            let task_view_button = self
                .task_rect(TaskItem::TaskView)
                .is_some_and(|r| r.contains(x, y));
            if self.tv.open && !task_view_button {
                self.close_task_view(false);
            }
            self.taskbar_press(x, y, right);
            return;
        }
        if self.tv.open {
            self.tv_press(x, y, right);
            return;
        }
        if let Some(app) = self.window_at(x, y) {
            self.focus(app);
            let w = self.windows[app.index()];
            if right && w.title_bar().contains(x, y) {
                self.title_menu(app, x, y);
            } else if !right && w.close_button().contains(x, y) {
                self.close(app);
            } else if !right && w.minimize_button().contains(x, y) {
                self.minimize(app);
            } else if !right && w.title_bar().contains(x, y) {
                self.drag = Some((app, x - w.rect.x, y - w.rect.y));
            } else if w.client().contains(x, y) {
                self.capture = Some(app);
                self.send_mouse(app, MouseKind::Down { right });
            }
            return;
        }
        // the desktop itself: icons and the selection rectangle
        self.focused_away();
        self.icons_press(x, y, right);
    }

    /// Right-click on a title bar.
    fn title_menu(&mut self, app: App, x: i32, y: i32) {
        let mut b = popup::Builder::default()
            .item("Minimize", Cmd::Minimize(app))
            .keyed("Close", "Alt+F4", Cmd::Close(app))
            .sep();
        for d in (0..self.desk_count).filter(|&d| d != self.current_desk) {
            let mut label = String::from("Move to ");
            label.push_str(desktops::desk_name(d).as_str());
            b = b.item(&label, Cmd::MoveTo(app, d));
        }
        let menu = b
            .maybe(
                "Move to new desktop",
                Cmd::MoveToNew(app),
                self.desk_count < desktops::MAX_DESKTOPS,
            )
            .at(x, y, false, self.screen());
        self.show_popup(menu);
    }

    fn held_move(&mut self) {
        if self.tv.drag.is_some() {
            self.tv_move(self.mouse_x, self.mouse_y);
        } else if self.desk_icons.busy() {
            self.icons_move(self.mouse_x, self.mouse_y);
        } else if let Some(s) = self.slider {
            self.drag_slider(s);
        } else if let Some((app, dx, dy)) = self.drag {
            self.move_window(app, self.mouse_x - dx, self.mouse_y - dy);
        } else if let Some(app) = self.capture {
            self.send_mouse(app, MouseKind::Move);
        }
    }

    fn release(&mut self) {
        if self.left || self.right {
            return;
        }
        self.drag = None;
        if self.slider.take() == Some(tray::Slider::Volume) {
            // let go of the volume: play a sound at the new loudness
            crate::sound::play(&crate::sound::volume_chime());
        }
        if let Some(app) = self.capture.take() {
            self.send_mouse(app, MouseKind::Up);
        }
        if self.tv.drag.is_some() {
            self.tv_release();
        }
        if self.desk_icons.busy() {
            self.icons_release();
        }
    }

    fn send_mouse(&mut self, app: App, kind: MouseKind) {
        let client = self.windows[app.index()].client();
        let ev = MouseEvent {
            x: self.mouse_x - client.x,
            y: self.mouse_y - client.y,
            kind,
        };
        let changed = match app {
            App::Paint => self.paint.on_mouse(ev),
            App::Calculator => self.calc.on_mouse(ev),
            App::Browser => self.browser.on_mouse(ev),
            App::Notepad => self.notepad.on_mouse(ev),
            App::Explorer => self.explorer.on_mouse(ev),
            App::Settings => self.settings.on_mouse(ev),
            App::About => self.about.on_mouse(ev),
            App::TaskManager => self.taskmgr.on_mouse(ev),
            App::Photos => self.photos.on_mouse(ev),
            App::Video => self.video.on_mouse(ev),
            App::Store => self.store.on_mouse(ev),
            App::Program => self.program.on_mouse(ev),
            App::Terminal => false,
        };
        if core::mem::take(&mut self.settings.switch_layout) {
            self.toggle_layout = true;
        }
        if changed {
            self.cursor_on = true;
            self.app_changed(app);
        }
    }

    // ---- tray ------------------------------------------------------------------

    /// Where a tray flyout sits: under the menu bar, at the right.
    fn panel_rect(&self, panel: Panel) -> Rect {
        let (w, h) = panel.size();
        let x = if panel == Panel::Hidden {
            // under its ^ button
            let b = self.tray_rect(3);
            b.x + b.w / 2 - w / 2
        } else {
            self.width - 8 - w
        };
        Rect::new(x, MENUBAR_H + 8, w, h)
    }

    fn damage_panel(&mut self) {
        if self.panel.is_some() {
            self.damage(self.panel_rect(self.panel_shown));
        }
    }

    fn open_panel(&mut self, panel: Panel) {
        self.hide_tip();
        self.close_menu();
        self.close_search();
        if self.panel.is_some() {
            self.damage(self.panel_rect(self.panel_shown).inset(-SPREAD));
        }
        if self.panel_shown != panel {
            // a different flyout slides in from the start
            self.panel_anim = Tween::new(0, 0, 1);
        }
        self.panel = Some(panel);
        self.panel_shown = panel;
        if panel == Panel::Quick {
            self.tray.update_net();
        }
        self.panel_anim.retarget(ONE, anim::ms(220));
        self.damage(self.panel_rect(panel).inset(-SPREAD));
        self.damage_taskbar();
    }

    fn close_panel(&mut self) {
        if self.panel.take().is_some() {
            self.slider = None;
            self.panel_anim.retarget(0, anim::ms(160));
            self.damage(self.panel_rect(self.panel_shown).inset(-SPREAD));
            self.damage_taskbar();
        }
    }

    fn quick_click(&mut self, r: Rect, x: i32, y: i32) {
        match self.tray.target_at(r, x, y) {
            Some(tray::Target::LayoutTile) => self.toggle_layout = true,
            Some(tray::Target::NetworkTile) => {
                // start the network if nothing has yet
                crate::net::init();
                self.tray.update_net();
                self.damage(r);
            }
            Some(tray::Target::Slider(s)) => {
                self.slider = Some(s);
                self.drag_slider(s);
            }
            None => {}
        }
    }

    fn drag_slider(&mut self, s: tray::Slider) {
        let r = self.panel_rect(Panel::Quick);
        if self.tray.drag(r, s, self.mouse_x) {
            self.damage(r);
            if s == tray::Slider::Brightness {
                self.present_all = true;
            } else {
                crate::sound::set_volume(self.tray.volume);
                self.damage_tray();
            }
        }
    }

    fn damage_tray(&mut self) {
        self.damage(self.tray_rect(3).union(&self.tray_rect(2)));
    }

    fn open_menu(&mut self) {
        self.hide_tip();
        self.close_panel();
        self.close_search();
        self.close_task_view(false);
        self.start.show();
        self.start
            .set_hover(self.menu_panel(), self.mouse_x, self.mouse_y);
        self.menu.retarget(ONE, anim::ms(220));
        self.damage(self.menu_rect());
        self.damage_taskbar();
    }

    fn close_menu(&mut self) {
        if self.start.open {
            self.start.open = false;
            self.menu.retarget(0, anim::ms(160));
            self.damage(self.menu_rect());
            self.damage_taskbar();
        }
    }

    fn menu_action(&mut self, action: start::Action) {
        match action {
            start::Action::None => {}
            start::Action::Redraw => self.damage(self.menu_rect()),
            start::Action::Close => self.close_menu(),
            start::Action::Open(app) => {
                self.close_menu();
                self.open(app);
            }
            start::Action::Program(i) => {
                let path = self.start.program(i).map(String::from);
                self.close_menu();
                if let Some(path) = path {
                    self.open_file(&path);
                }
            }
            start::Action::GetPrograms => {
                self.close_menu();
                self.open(App::Store);
            }
            start::Action::Restart => {
                self.close_menu();
                self.power(power::Power::Restart);
            }
            start::Action::ShutDown => {
                self.close_menu();
                self.power(power::Power::ShutDown);
            }
            start::Action::Lock => self.lock(false),
            start::Action::SignOut => self.lock(true),
        }
    }

    fn menu_panel(&self) -> Rect {
        StartMenu::panel(self.width, self.height, TASKBAR_H, MENUBAR_H)
    }

    /// The start menu, with room for its shadow.
    fn menu_rect(&self) -> Rect {
        self.menu_panel().inset(-SPREAD)
    }

    /// The dock with its shadow, and the menu bar (it names the app in
    /// front).
    fn damage_taskbar(&mut self) {
        let top = self.height - TASKBAR_H - SPREAD;
        self.damage(Rect::new(0, top, self.width, TASKBAR_H + SPREAD));
        self.damage(Rect::new(0, 0, self.width, MENUBAR_H + 1));
    }

    // ---- drawing -----------------------------------------------------------

    fn render(&mut self) {
        let fading = match self.phase {
            Phase::Unlocking(t) | Phase::Locking(t) => Some(t),
            _ => self.crossfade,
        };
        if self.dirty.is_empty() && fading.is_none() && self.slide.is_none() && !self.present_all {
            return;
        }
        self.update_surfaces();
        let (rects, n) = self.dirty.take();
        let mut back = core::mem::take(&mut self.back);
        let mut scratch = core::mem::take(&mut self.scratch);
        for r in &rects[..n] {
            let mut c = Canvas::new(back, self.width as usize, self.height as usize);
            c.clip_to(*r);
            self.draw_scene(&mut c, scratch);
            if !matches!(self.phase, Phase::Boot(_) | Phase::Power(..)) {
                self.pointer_image.draw(&mut c, self.mouse_x, self.mouse_y);
            }
        }
        core::mem::swap(&mut self.back, &mut back);
        core::mem::swap(&mut self.scratch, &mut scratch);
        if let Some((t, dir)) = self.slide {
            self.present_slide(t.value(), dir);
            self.present_all = false;
            return;
        }
        match fading {
            // the old screen over the new one, fading out
            Some(t) => self.present_fade(Self::crossfade_alpha(t)),
            None if self.present_all => self.present(self.screen()),
            None => {
                for r in &rects[..n] {
                    self.present(*r);
                }
            }
        }
        self.present_all = false;
    }

    /// Facts about the system for Settings and About.
    fn system_info(&self) -> settings::Info<'_> {
        settings::Info {
            screen: (self.width, self.height),
            memory_mib: self.boot.upper_memory_kib / 1024 + 1,
            bootloader: self.boot.bootloader,
            net: self.tray.net,
            address: self.tray.address.as_str(),
            layout: self.layout,
            clock: self.clock.as_str(),
            date: self.date.as_str(),
            uptime_minutes: interrupts::ticks() / interrupts::TIMER_HZ / 60,
        }
    }

    /// Bring stale window contents up to date.
    fn update_surfaces(&mut self) {
        let surfaces = core::mem::take(&mut self.surfaces);
        for app in APPS {
            if self.stale[app.index()] && self.windows[app.index()].drawn() {
                self.stale[app.index()] = false;
                let (w, h) = app.client_size();
                let mut c = Canvas::new(surfaces[app.index()], w as usize, h as usize);
                let focused = self.focused == Some(app);
                match app {
                    App::Terminal => self.terminal.draw(&mut c, focused && self.cursor_on),
                    App::Paint => self.paint.draw(&mut c),
                    App::Calculator => self.calc.draw(&mut c),
                    App::Photos => self.photos.draw(&mut c),
                    App::Video => self.video.draw(&mut c),
                    App::Store => self.store.draw(&mut c),
                    App::Program => self.program.draw(&mut c),
                    App::Browser => self.browser.draw(&mut c),
                    App::Notepad => self.notepad.draw(&mut c, focused && self.cursor_on),
                    App::Explorer => self.explorer.draw(&mut c, focused && self.cursor_on),
                    App::Settings => self.settings.draw(&mut c, &self.system_info()),
                    App::About => self.about.draw(&mut c, &self.system_info()),
                    App::TaskManager => {
                        let rows = self.task_rows();
                        self.taskmgr.set_rows(rows);
                        self.taskmgr.draw(&mut c);
                    }
                }
            }
        }
        self.surfaces = surfaces;
    }

    /// Draw everything but the pointer inside the canvas's clip.
    fn draw_scene(&self, c: &mut Canvas, scratch: &mut [u32]) {
        match self.phase {
            Phase::Boot(since) => return self.draw_boot(c, since),
            Phase::Power(what, since) => return self.draw_power(c, what, since),
            _ => {}
        }
        if matches!(self.phase, Phase::Login | Phase::Locking(_)) {
            self.login.draw(c, self.wallpaper);
            return;
        }
        if self.tv.open {
            self.draw_task_view(c, scratch);
        } else {
            c.blit(
                0,
                0,
                self.width,
                self.height,
                self.wallpaper,
                self.width as usize,
            );
            self.draw_desk_icons(c);
            self.draw_icon_rename(c);
            for i in 0..self.order_len {
                let app = self.order[i];
                if self.windows[app.index()].drawn() {
                    self.draw_window(c, app, scratch);
                }
            }
        }
        self.draw_taskbar(c);
        if self.start.open || self.menu.value() > 0 {
            self.draw_menu(c, scratch);
        }
        if self.search.open {
            let top = self.top_apps();
            let (p, blink) = (self.search_panel(), self.cursor_on);
            self.search.draw(c, p, self.icons, &top, &self.pins, blink);
        }
        if self.panel.is_some() || self.panel_anim.value() > 0 {
            self.draw_panel(c, scratch);
        }
        self.draw_switcher(c, scratch);
        if self.confirm_empty {
            let area = Rect::new(0, 0, self.width, self.height - TASKBAR_H);
            widgets::draw_message(
                c,
                area,
                "Delete Multiple Items",
                &CONFIRM_LINES,
                &["Yes", "No"],
                None,
            );
        }
        if let Some(p) = &self.popup {
            p.draw(c);
        }
        self.draw_icon_drag(c);
        self.draw_tip(c);
    }

    /// Copy part of the back buffer to the screen.
    fn present(&self, r: Rect) {
        let fb = &self.fb;
        let stride = self.width as usize;
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        let dim = self.dim();
        let mut dimmed = [0u32; MAX_W];
        for y in r.y as usize..r.bottom() as usize {
            let mut row = &self.back[y * stride + r.x as usize..y * stride + r.right() as usize];
            if dim > 0 {
                let out = &mut dimmed[..row.len()];
                fade_row(out, row, &BLACK[..row.len()], dim);
                row = out;
            }
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch + r.x as usize * 4) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, row.len());
                }
            } else {
                for (i, &p) in row.iter().enumerate() {
                    put_pixel(fb, r.x as usize + i, y, p);
                }
            }
        }
    }

    /// How much to darken the picture for the brightness setting, 0 to 256.
    fn dim(&self) -> u32 {
        ((100 - self.tray.brightness.clamp(tray::MIN_BRIGHTNESS, 100)) * 256 / 100) as u32
    }

    /// Show the snapshot blended over the back buffer with `alpha` (0 to
    /// 256), for the fade between the sign-in screen and the desktop.
    fn present_fade(&self, alpha: u32) {
        let fb = &self.fb;
        let (w, h) = (self.width as usize, self.height as usize);
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        let mut row = [0u32; MAX_W];
        let dim = self.dim();
        for y in 0..h {
            let (back, snap) = (&self.back[y * w..][..w], &self.snapshot[y * w..][..w]);
            fade_row(&mut row[..w], back, snap, alpha);
            if dim > 0 {
                let faded = row;
                fade_row(&mut row[..w], &faded[..w], &BLACK[..w], dim);
            }
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, w);
                }
            } else {
                for (x, &p) in row[..w].iter().enumerate() {
                    put_pixel(fb, x, y, p);
                }
            }
        }
    }

    fn draw_window(&self, c: &mut Canvas, app: App, scratch: &mut [u32]) {
        let w = self.windows[app.index()];
        let focused = self.focused == Some(app);
        let strength = if focused { 120 } else { 70 };
        let border = theme::window_border(focused);
        let r = w.rect;
        let size = (r.w * r.h) as usize;
        let Some((frame, alpha)) = self.anim_frame(app).filter(|_| size <= scratch.len()) else {
            if !c.visible(w.bounds()) || !w.visible() {
                return;
            }
            c.shadow(r, WINDOW_RADIUS, SPREAD, SHADOW_DROP, strength);
            {
                let mut win = c.sub(Rect::new(0, 0, c.width, c.height));
                win.clip_round(r, WINDOW_RADIUS);
                self.draw_window_body(&mut win, app, r);
            }
            c.outline_round(r, WINDOW_RADIUS, border);
            return;
        };
        // moving: draw it at full size on the side, then scale and blend
        if frame.w <= 0 || frame.h <= 0 || !c.visible(shadow_bounds(frame)) {
            return;
        }
        c.shadow(
            frame,
            WINDOW_RADIUS,
            SPREAD,
            SHADOW_DROP,
            strength * alpha / ONE,
        );
        {
            let mut side = Canvas::new(scratch, r.w as usize, r.h as usize);
            self.draw_window_body(&mut side, app, Rect::new(0, 0, r.w, r.h));
        }
        {
            let mut win = c.sub(Rect::new(0, 0, c.width, c.height));
            win.clip_round(frame, WINDOW_RADIUS);
            win.blit_scaled(frame, scratch, r.w, r.h, alpha);
        }
        c.outline_round_alpha(frame, WINDOW_RADIUS, border, alpha);
    }

    /// The title bar and contents of a window whose frame is `r` on `win`.
    fn draw_window_body(&self, win: &mut Canvas, app: App, r: Rect) {
        let focused = self.focused == Some(app);
        let title_face = if focused {
            theme::title_active()
        } else {
            theme::face()
        };
        let title_bar = Rect::new(r.x, r.y, r.w, TITLE_H);
        win.fill(title_bar, title_face);
        let ink = if focused {
            theme::text()
        } else {
            theme::text_dim()
        };
        // the icon and title in the middle, clear of the buttons
        let title = self.window_title(app);
        let room = r.w - 2 * 76;
        let title = search::fit(&title, room - 24);
        let tw = text::UI.width(&title) + 24;
        let tx = r.x + (r.w - tw) / 2;
        self.icons.draw_small(win, app, tx, r.y + 8);
        win.draw_text(tx + 24, r.y + 8, &title, ink);

        // round caption buttons: coral closes, amber minimises; grey on
        // windows in the back until the mouse comes near
        let near = self
            .hover
            .level(Hover::Close(app))
            .max(self.hover.level(Hover::Minimize(app)));
        for (k, hover, color) in [
            (0, Hover::Close(app), rgb(0xf2, 0x5c, 0x54)),
            (1, Hover::Minimize(app), rgb(0xff, 0x9f, 0x43)),
        ] {
            let b = caption_button(r, k);
            let dot = Rect::new(b.x + 4, b.y + 4, 14, 14);
            let face = if focused || near > 0 {
                color
            } else {
                mix(title_face, theme::thumb(), 150)
            };
            win.fill_round(dot, 7, face);
            win.outline_round_alpha(dot, 7, mix(face, 0, 60), ONE / 2);
            let lit = self.hover.level(hover).max(near / 2) as u32;
            if lit > 0 {
                let ink = mix(face, rgb(0x3a, 0x1a, 0x10), lit * 255 / 256);
                let (cx, cy) = (dot.x + 7, dot.y + 7);
                if k == 0 {
                    win.line(cx - 3, cy - 3, cx + 3, cy + 3, ink);
                    win.line(cx + 3, cy - 3, cx - 3, cy + 3, ink);
                } else {
                    win.fill_rect(cx - 3, cy, 7, 1, ink);
                }
            }
        }

        let client = Rect::new(
            r.x + BORDER,
            r.y + TITLE_H,
            r.w - 2 * BORDER,
            r.h - TITLE_H - BORDER,
        );
        let surface = &self.surfaces[app.index()];
        win.blit(
            client.x,
            client.y,
            client.w,
            client.h,
            surface,
            client.w as usize,
        );
    }

    /// The start menu, sliding up and fading in while it opens.
    fn draw_menu(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let blink = self.cursor_on && self.start.open;
        let panel = self.menu_panel();
        self.draw_sliding(c, scratch, panel, self.menu.value(), true, |c, p| {
            self.start.draw(c, p, self.icons, blink)
        });
    }

    fn draw_panel(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let panel = self.panel_shown;
        let hover = match self.hover.lit()[0] {
            Some(Hover::Quick(t)) => Some(t),
            _ => None,
        };
        let r = self.panel_rect(panel);
        let shown = self.panel_anim.value() >= ONE;
        if shown {
            // in place: the shadow and frame the sliding version adds
            c.shadow(r, 8, SPREAD, 4, 120);
        }
        self.draw_sliding(c, scratch, r, self.panel_anim.value(), false, |c, p| {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, 8);
            match panel {
                Panel::Quick => self.tray.draw_quick(&mut m, p, self.layout, hover),
                Panel::Calendar => Tray::draw_calendar(&mut m, p),
                Panel::Hidden => self.draw_hidden_icons(&mut m, p),
            }
        });
        if shown {
            c.outline_round(r, 8, theme::frame());
        }
    }

    /// A flyout `panel` that is `p` of the way in (ONE is fully shown):
    /// see-through while it slides up out of the dock (`up`) or down out
    /// of the menu bar.
    fn draw_sliding(
        &self,
        c: &mut Canvas,
        scratch: &mut [u32],
        panel: Rect,
        p: i32,
        up: bool,
        draw: impl Fn(&mut Canvas, Rect),
    ) {
        if p >= ONE {
            draw(c, panel);
            return;
        }
        let travel = if up { 48 } else { -24 };
        let r = panel.inset(-SPREAD);
        if !c.visible(r.union(&r.offset(0, travel))) || p <= 0 {
            return;
        }
        let frame = panel.offset(0, (ONE - p) * travel / ONE);
        // it comes out from behind the dock or the menu bar
        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
        m.clip_to(if up {
            Rect::new(0, 0, self.width, self.height - TASKBAR_H)
        } else {
            Rect::new(0, MENUBAR_H, self.width, self.height - MENUBAR_H)
        });
        m.shadow(frame, 8, SPREAD, 4, 120 * p / ONE);
        {
            let mut side = Canvas::new(scratch, panel.w as usize, panel.h as usize);
            draw(&mut side, Rect::new(0, 0, panel.w, panel.h));
        }
        {
            let mut inner = m.sub(Rect::new(0, 0, self.width, self.height));
            inner.clip_round(frame, 8);
            inner.blit_scaled(frame, scratch, panel.w, panel.h, p);
        }
        m.outline_round_alpha(frame, 8, theme::frame(), p);
    }
}

/// What asking before emptying the Recycle Bin says.
const CONFIRM_LINES: [&str; 1] =
    ["Are you sure you want to permanently delete everything in the Trash?"];

// ---- pictures ---------------------------------------------------------------

/// The mouse pointer, drawn at 4x with polygons and shrunk with alpha
/// so its edges are smooth.
struct Pointer {
    pixels: [u32; POINTER_W * POINTER_H],
}

const POINTER_W: usize = 16;
const POINTER_H: usize = 24;

impl Pointer {
    fn new() -> Self {
        // the arrow outline and its white inside, in 1/4 pixels
        const OUTER: [(i32, i32); 7] = [
            (2, 2),
            (2, 82),
            (21, 64),
            (35, 94),
            (48, 88),
            (35, 60),
            (60, 60),
        ];
        const INNER: [(i32, i32); 7] = [
            (8, 16),
            (8, 68),
            (22, 55),
            (37, 86),
            (41, 84),
            (27, 54),
            (45, 54),
        ];
        const S: usize = 4;
        let mut big = [0u32; POINTER_W * S * POINTER_H * S];
        let marker = 0xff00_0000;
        big.fill(marker);
        let mut c = Canvas::new(&mut big, POINTER_W * S, POINTER_H * S);
        c.fill_polygon(&OUTER, 0x000000);
        c.fill_polygon(&INNER, 0xffffff);
        let mut pixels = [0u32; POINTER_W * POINTER_H];
        for (i, out) in pixels.iter_mut().enumerate() {
            let (ox, oy) = (i % POINTER_W, i / POINTER_W);
            let (mut sum, mut count) = (0u32, 0u32);
            for y in oy * S..oy * S + S {
                for x in ox * S..ox * S + S {
                    let p = big[y * POINTER_W * S + x];
                    if p != marker {
                        sum += p & 0xff;
                        count += 1;
                    }
                }
            }
            if let Some(grey) = sum.checked_div(count) {
                *out = (count * 255 / (S * S) as u32) << 24 | grey << 16 | grey << 8 | grey;
            }
        }
        Self { pixels }
    }

    fn draw(&self, c: &mut Canvas, x: i32, y: i32) {
        c.blit_alpha(x, y, POINTER_W as i32, POINTER_H as i32, &self.pixels);
    }
}

fn pointer_rect(x: i32, y: i32) -> Rect {
    Rect::new(x, y, POINTER_W as i32, POINTER_H as i32)
}

/// A row of black, to dim towards.
static BLACK: [u32; MAX_W] = [0; MAX_W];

/// `out = mix(a, b, alpha)` for a row, two pixels per step: each 64-bit
/// word holds the red and blue (or green) channels of both, so one
/// multiply blends four channels.
fn fade_row(out: &mut [u32], a: &[u32], b: &[u32], alpha: u32) {
    const M: u64 = 0x00ff_00ff_00ff_00ff;
    let (t, s) = (alpha.min(256) as u64, 256 - alpha.min(256) as u64);
    let pairs = out.len() / 2;
    for i in 0..pairs {
        let pa = a[2 * i] as u64 | (a[2 * i + 1] as u64) << 32;
        let pb = b[2 * i] as u64 | (b[2 * i + 1] as u64) << 32;
        let rb = (((pa & M) * s + (pb & M) * t) >> 8) & M;
        let g = ((((pa >> 8) & M) * s + ((pb >> 8) & M) * t) >> 8) & M;
        let p = rb | g << 8;
        out[2 * i] = p as u32 & 0xff_ffff;
        out[2 * i + 1] = (p >> 32) as u32 & 0xff_ffff;
    }
    if out.len() % 2 == 1 {
        let i = out.len() - 1;
        out[i] = fast_mix(a[i], b[i], alpha);
    }
}

/// Caption button `k` (0 close, 1 minimise) of a window framed by `r`.
fn caption_button(r: Rect, k: i32) -> Rect {
    Rect::new(r.x + 10 + k * 22, r.y + 5, 22, 22)
}

/// Write one pixel to a framebuffer that is not 32-bit XRGB.
fn put_pixel(fb: &Framebuffer, x: usize, y: usize, p: u32) {
    let color = crate::framebuffer::Rgb::new((p >> 16) as u8, (p >> 8) as u8, p as u8);
    fb.put_raw(x, y, fb.encode(color));
}

/// A copy of the wallpaper the size of a Task View desktop picture.
fn shrink_wallpaper(thumb: &mut [u32], wallpaper: &[u32], width: i32, height: i32) {
    Canvas::new(thumb, desktops::TILE_W as usize, desktops::TILE_H as usize).blit_smooth(
        Rect::new(0, 0, desktops::TILE_W, desktops::TILE_H),
        wallpaper,
        width,
        height,
    );
}

/// Deep blue with a soft flower of light in the middle.
fn draw_wallpaper(c: &mut Canvas) {
    let (w, h) = (c.width, c.height);
    c.vertical_gradient(
        Rect::new(0, 0, w, h),
        rgb(0x0a, 0x2c, 0x74),
        rgb(0x1c, 0x5c, 0xc0),
    );
    let (cx, cy) = (w / 2, (h - TASKBAR_H) / 2 + 20);
    // unit vectors for eight directions, times 100
    const DIRS: [(i32, i32); 8] = [
        (100, 0),
        (71, 71),
        (0, 100),
        (-71, 71),
        (-100, 0),
        (-71, -71),
        (0, -100),
        (71, -71),
    ];
    let petal = h / 5;
    for (i, (dx, dy)) in DIRS.into_iter().enumerate() {
        let (px, py) = (cx + dx * petal * 3 / 400, cy + dy * petal * 3 / 400);
        let color = if i % 2 == 0 {
            rgb(0x6a, 0xb4, 0xff)
        } else {
            rgb(0x9a, 0xcc, 0xff)
        };
        let r = Rect::new(px - petal, py - petal, 2 * petal, 2 * petal);
        c.fill_round_alpha(r, petal, color, 46);
    }
    for (radius, alpha) in [(petal * 3 / 4, 50), (petal / 2, 70), (petal / 4, 90)] {
        let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
        c.fill_round_alpha(r, radius, rgb(0xe4, 0xf2, 0xff), alpha);
    }
}

/// Paint the whole screen black at once, so the boot messages go away
/// while the desktop gets ready.
fn clear_screen(fb: &Framebuffer) {
    for y in 0..fb.height {
        for x in 0..fb.width {
            fb.put_raw(x, y, 0);
        }
    }
}

// ---- power ------------------------------------------------------------------

fn restart() {
    // pulse the CPU reset line through the PS/2 controller
    unsafe { port::outb(0x64, 0xfe) };
}

fn shut_down() {
    // ACPI power off in QEMU (PIIX4 and ICH9) and Bochs
    unsafe {
        port::outw(0x604, 0x2000);
        port::outw(0xb004, 0x2000);
        port::outw(0x4004, 0x3400);
    }
}

// ---- main loop ----------------------------------------------------------------

/// Run the desktop forever.
pub fn run(fb: Framebuffer, boot: &BootInfo) -> ! {
    clear_screen(&fb);
    let mut desk = Desktop::new(fb, boot);
    // the boot animation starts once everything is ready to draw
    desk.phase = Phase::Boot(interrupts::ticks());
    // the shell now prints into the terminal window
    CONSOLE.lock().detach(terminal::COLS, terminal::ROWS);
    ACTIVE.store(true, Ordering::Relaxed);
    crate::print_banner();
    desk.terminal.start();
    // the boot screen, then the lock screen; the terminal opens after
    // signing in
    desk.damage(desk.screen());

    let mut keyboard = Keyboard::new();
    let mut mouse = ps2::MouseDecoder::new();
    let absolute = vmmouse::init();
    desk.absolute = absolute;
    serial::write_str(if absolute {
        "desktop: absolute mouse\n"
    } else {
        "desktop: PS/2 mouse\n"
    });
    // bring the network up now, so the taskbar can show it
    crate::net::init();
    let mut next_blink = 0;
    let mut last_second = u64::MAX;
    loop {
        while let Some(scancode) = KEYBOARD_BYTES.pop() {
            if let Some(key) = keyboard.feed(scancode) {
                desk.layout = keyboard.layout();
                desk.on_key(key);
            }
        }
        while let Some(byte) = MOUSE_BYTES.pop() {
            if let Some(packet) = mouse.feed(byte) {
                desk.on_ps2(packet);
            }
        }
        if absolute {
            while let Some(ev) = vmmouse::poll() {
                desk.on_vmmouse(ev);
            }
        }
        while let Some(request) = REQUESTS.pop() {
            let app = APPS[(request & !CLOSE) as usize % APPS.len()];
            if request == LOCK {
                desk.lock(false);
            } else if request == RESTART || request == SHUT_DOWN {
                if !matches!(desk.phase, Phase::Boot(_)) {
                    desk.power(if request == RESTART {
                        power::Power::Restart
                    } else {
                        power::Power::ShutDown
                    });
                }
            } else if request & CLOSE != 0 {
                desk.close(app);
            } else {
                desk.open(app);
            }
        }

        if core::mem::take(&mut desk.toggle_layout) {
            keyboard.toggle_layout();
            desk.on_key(Key::LayoutChanged);
        }
        desk.layout = keyboard.layout();
        desk.poll_apps();
        if let Some(background) = personalize::take_changed() {
            desk.apply_look(background);
        }
        if matches!(desk.phase, Phase::Desktop) {
            desk.refresh_icons();
        }
        crate::net::poll();
        if desk.browser.tick() {
            desk.damage_client(App::Browser);
        }
        if desk.windows[App::Photos.index()].open && desk.photos.tick() {
            desk.app_changed(App::Photos);
        }
        if desk.windows[App::Video.index()].open && desk.video.tick() {
            desk.app_changed(App::Video);
        }
        if desk.store.busy() && desk.store.tick() {
            desk.app_changed(App::Store);
        }
        if desk.windows[App::TaskManager.index()].open && desk.taskmgr.tick() {
            desk.damage_client(App::TaskManager);
        }
        if desk.windows[App::Program.index()].open && desk.program.tick() {
            let title = desk.program.program_title();
            if title != desk.program_title {
                desk.program_title = title;
                // the title bar and the menu bar show the program's name
                desk.app_changed(App::Program);
                desk.damage_taskbar();
            } else {
                desk.damage_client(App::Program);
            }
        }
        if CONSOLE.lock().take_changed() {
            desk.damage_client(App::Terminal);
        }
        desk.login.layout = desk.layout.name();
        desk.tick();
        let on_desktop = matches!(desk.phase, Phase::Desktop);
        let now = interrupts::ticks();
        if now >= next_blink {
            next_blink = now + BLINK_TICKS;
            desk.cursor_on = !desk.cursor_on;
            if !on_desktop {
            } else if desk.start.open {
                // the caret in the search box
                desk.damage(desk.menu_rect());
            } else if desk.search.open {
                desk.damage(Search::field(desk.search_panel()));
            } else if let Some((i, _)) = &desk.desk_icons.renaming {
                let r = desk.icon_rect(*i).inset(-8);
                desk.damage(r);
            } else if let Some(app @ (App::Terminal | App::Notepad | App::Explorer)) = desk.focused
            {
                // the text caret blinks
                desk.stale[app.index()] = true;
                desk.damage_client(app);
            }
        }
        let second = now / interrupts::TIMER_HZ;
        if second != last_second {
            last_second = second;
            let (h, m, _) = rtc::time();
            let (year, month, day) = rtc::date();
            let mut clock = StackString::<16>::new();
            let _ = write!(clock, "{:02}:{:02}", h, m);
            let changed = clock.as_str() != desk.clock.as_str();
            desk.clock = clock;
            desk.date.clear();
            let _ = write!(desk.date, "{:02}.{:02}.{}", day, month, year);
            let net_changed = desk.tray.update_net();
            if on_desktop {
                if changed || net_changed {
                    desk.damage_tray();
                    desk.damage_client(App::Settings);
                }
                if net_changed {
                    desk.damage_panel();
                }
            } else {
                desk.login.update_clock();
            }
        }

        desk.render();
        // pages loading in the background keep the loop going
        let busy = desk.browser.busy()
            || desk.program.busy()
            || (desk.windows[App::Photos.index()].open && desk.photos.busy())
            || (desk.windows[App::Video.index()].open && desk.video.busy())
            || desk.store.busy();
        interrupts::wait_for_interrupt(|| {
            busy || !KEYBOARD_BYTES.is_empty() || !MOUSE_BYTES.is_empty() || !REQUESTS.is_empty()
        });
    }
}
