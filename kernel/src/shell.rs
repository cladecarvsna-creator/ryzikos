//! A tiny command line: reads keys, echoes them and runs commands.

use alloc::string::String;

use crate::console::{self, Color, CONSOLE};
use crate::gui::{self, App};
use crate::interrupts;
use crate::keyboard::Key;
use crate::multiboot::BootInfo;
use crate::{archive, fs, print, println, users};

const MAX_LINE: usize = 1000;

pub struct Shell {
    line: [char; MAX_LINE],
    len: usize,
    /// The previous command, recalled with the Up arrow.
    last: [char; MAX_LINE],
    last_len: usize,
    /// The current folder; empty means the user's home.
    cwd: String,
}

impl Shell {
    pub const fn new() -> Self {
        Self {
            line: ['\0'; MAX_LINE],
            len: 0,
            last: ['\0'; MAX_LINE],
            last_len: 0,
            cwd: String::new(),
        }
    }

    pub fn prompt(&self) {
        console::print_colored(Color::LightGreen, format_args!("ryzikos"));
        console::print_colored(Color::LightGray, format_args!("> "));
    }

    pub fn on_key(&mut self, key: Key, boot: &BootInfo) {
        match key {
            Key::Char(c) if self.len < MAX_LINE => {
                self.line[self.len] = c;
                self.len += 1;
                print!("{}", c);
            }
            Key::Backspace if self.len > 0 => {
                self.len -= 1;
                CONSOLE.lock().backspace();
            }
            Key::Up => {
                self.erase_line();
                self.line = self.last;
                self.len = self.last_len;
                for &c in &self.line[..self.len] {
                    print!("{}", c);
                }
            }
            Key::Escape => self.erase_line(),
            // Ctrl+V pastes; Ctrl+Shift+C copies the screen (Ctrl+C
            // stops the line, as in other terminals)
            Key::Ctrl('v') => {
                let clip = crate::gui::clipboard_text();
                for c in clip.chars().filter(|c| !c.is_control()) {
                    if self.len == MAX_LINE {
                        break;
                    }
                    self.line[self.len] = c;
                    self.len += 1;
                    print!("{}", c);
                }
            }
            Key::Ctrl('c') if crate::keyboard::shift_held() => {
                crate::gui::copy_text(&CONSOLE.lock().text());
            }
            Key::Ctrl('c') => {
                println!("^C");
                self.len = 0;
                self.prompt();
            }
            Key::Ctrl('l') => {
                CONSOLE.lock().clear();
                self.len = 0;
                self.prompt();
            }
            Key::Enter => {
                println!();
                if self.len > 0 {
                    self.last = self.line;
                    self.last_len = self.len;
                }
                let mut buf = [0u8; MAX_LINE * 4];
                let mut used = 0;
                for &c in &self.line[..self.len] {
                    used += c.encode_utf8(&mut buf[used..]).len();
                }
                self.len = 0;
                let line = core::str::from_utf8(&buf[..used]).unwrap_or("");
                self.run(line, boot);
                self.prompt();
            }
            _ => {}
        }
    }

    fn erase_line(&mut self) {
        let mut con = CONSOLE.lock();
        for _ in 0..self.len {
            con.backspace();
        }
        self.len = 0;
    }

    fn run(&mut self, line: &str, boot: &BootInfo) {
        let line = line.trim();
        let (command, args) = line.split_once(' ').unwrap_or((line, ""));
        match command {
            "" => {}
            "help" => {
                println!("Commands:");
                println!("  help    this list");
                println!("  clear   clear the screen (also Ctrl+L)");
                println!("  echo    print the arguments");
                println!("  info    screen, memory and uptime");
                println!("  paint   open Draw");
                println!("  calc    open the calculator");
                println!("  photos  open Photos (photos <picture> shows one)");
                println!("  video   open Video Player (video <file.mp4> plays one)");
                println!("  disc    look for a CD or DVD and list it (it is at /Disc)");
                println!("  drives  list the disks and CD/DVD drives");
                println!("  devices list the PC's hardware and which parts have drivers");
                println!("  telegram open Telegram (telegram selftest checks its crypto)");
                println!("  vpn     open VPN (vpn help lists add, list, on, off, test, log)");
                println!("  install install RyzikOS on a hard disk (from the live CD)");
                println!("  update  show the version and a downloaded update (update undo removes it)");
                println!("  beep    play the volume sound on the ES1370 sound card");
                println!("  store   open the App Store to install programs");
                println!("  open    open a file or run a program: open ~/Programs/snake.rzapp");
                println!("  zip     zip <archive.zip> <files or folders> packs them (Archiver does it too)");
                println!("  unzip   unzip <archive> [folder] unpacks ZIP, TAR, GZIP; unzip -l lists it");
                println!("  browser open the web browser (browser <address> goes there)");
                println!("  fetch   download a web page and show its title and links");
                println!("  exit    close the terminal window");
                println!("  whoami  show who is signed in; 'users' lists everyone");
                println!("  useradd add a user: useradd <name> [password]");
                println!("  passwd  set a password: passwd [<name>] <password>");
                println!("  lock    show the lock screen");
                println!("  ls      list a folder; cd, pwd, mkdir, rm, cat work with files");
                println!("  echo    echo <text> > <file> writes a file");
                println!("  notepad open Text Editor (notepad <file> opens a file)");
                println!("  explorer open Files (explorer <folder>)");
                println!("  settings open Settings; 'about' shows About RyzikOS");
                println!("  theme   theme light | theme dark");
                println!("  screen  show or change the resolution: screen 1920x1200");
                println!("  wallpaper <picture> or 'wallpaper next' changes the background");
                println!("  restart restart the computer; 'shutdown' turns it off");
                println!("  colors  show the text colours");
                println!("  panic   show the blue screen and restart; 'panic memory' makes a CPU fault");
                println!("  crash   why the computer restarted: the last crash report");
                println!("Keys: Alt+Shift switches EN/RU, Up recalls the last command.");
            }
            "clear" => CONSOLE.lock().clear(),
            "echo" => match args.split_once(" > ") {
                Some((text, file)) => {
                    let mut data = String::from(text);
                    data.push_str("\r\n");
                    report(fs::write(&self.path(file), data.as_bytes()));
                }
                None => println!("{}", args),
            },
            "pwd" => println!("{}", fs::display(&self.path(""))),
            "cd" => {
                let path = self.path(args);
                if fs::is_dir(&path) {
                    self.cwd = path;
                } else {
                    error("no such folder");
                }
            }
            "ls" | "dir" => ls(&self.path(args)),
            "cat" | "type" => match fs::read(&self.path(args)) {
                Ok(data) => {
                    let text = String::from_utf8_lossy(&data).replace('\r', "");
                    print!("{}", text);
                    if !text.ends_with('\n') {
                        println!();
                    }
                }
                Err(e) => error(e.message()),
            },
            "mkdir" | "md" => report(fs::create_dir(&self.path(args))),
            "rm" | "del" => report(fs::remove(&self.path(args))),
            "notepad" => {
                if !args.trim().is_empty() {
                    gui::request_file(&self.path(args));
                }
                open(App::Notepad);
            }
            "explorer" => {
                if !args.trim().is_empty() {
                    gui::request_folder(&self.path(args));
                }
                open(App::Explorer);
            }
            "settings" => open(App::Settings),
            "about" | "winver" => open(App::About),
            "info" => info(boot),
            "colors" => colors(),
            "paint" => open(App::Paint),
            "calc" => open(App::Calculator),
            "photos" | "video" => {
                let app = if command == "photos" { App::Photos } else { App::Video };
                if !args.trim().is_empty() {
                    gui::request_file(&self.path(args));
                }
                open(app);
            }
            "store" | "apps" => open(App::Store),
            "beep" => {
                if crate::sound::available() {
                    crate::sound::play(&crate::sound::volume_chime());
                } else {
                    error("No sound card: start QEMU with -device ES1370");
                }
            }
            "open" | "run" => {
                // the desktop picks the app: programs get their own window
                if args.trim().is_empty() {
                    error("Usage: open <file>");
                } else if fs::exists(&self.path(args)) {
                    gui::request_file(&self.path(args));
                } else {
                    error("File not found");
                }
            }
            "archiver" | "7z" | "winrar" => {
                if !args.trim().is_empty() {
                    gui::request_file(&self.path(args));
                }
                open(App::Archiver);
            }
            "zip" => self.zip(args),
            "unzip" | "untar" => self.unzip(args),
            "telegram" | "tg" => {
                if args.trim() == "selftest" {
                    match crate::tg::self_test() {
                        Ok(()) => {
                            println!("Telegram self-test passed.");
                            crate::serial::write_str("\ntelegram: self-test ok\n");
                        }
                        Err(what) => {
                            println!("Telegram self-test failed: {}", what);
                            crate::serial::write_str("\ntelegram: self-test FAILED\n");
                        }
                    }
                } else if args.trim() == "log" {
                    let lines = crate::tg::client::recent_log();
                    if lines.is_empty() {
                        println!("Telegram has not done anything yet.");
                    }
                    for l in lines {
                        println!("{}", l);
                    }
                } else {
                    open(App::Telegram);
                }
            }
            "install" => {
                // `install` opens the installer; `install erase 1` or
                // `install keep 1` installs on Disk 1 without asking
                let words: alloc::vec::Vec<&str> = args.split_whitespace().collect();
                if !crate::install::available() {
                    error("Only the live CD can install RyzikOS");
                } else if words.is_empty() {
                    open(App::Installer);
                } else if let (Some(&how @ ("erase" | "keep")), Some(Ok(n))) =
                    (words.first(), words.get(1).map(|n| n.parse::<usize>()))
                {
                    let result = if n == 0 {
                        Err(alloc::string::String::from("disks are numbered from 1"))
                    } else {
                        crate::install::install(n - 1, how == "erase", |_| {})
                    };
                    match result {
                        Ok(()) => println!("RyzikOS is installed on Disk {}. Restart to start it.", n),
                        Err(e) => error(&e),
                    }
                } else {
                    error("Usage: install, or install erase|keep <disk number>");
                }
            }
            "update" => {
                println!("RyzikOS {}", crate::update::version());
                match (crate::update::installed(), args.trim()) {
                    (Some(b), "undo") => {
                        crate::update::remove();
                        println!("Removed update {}: the disc's own RyzikOS starts next time", b);
                    }
                    (Some(b), _) if b == crate::update::BUILD => {
                        println!("Running update {} from the disk", b)
                    }
                    (Some(b), _) => println!("Update {} is on the disk; restart to use it", b),
                    (None, _) => println!("No update downloaded. Settings > Update looks for one"),
                }
            }
            "devices" | "lspci" => {
                // what is in this PC, and what RyzikOS can use
                for (d, vendor, device, class, sub) in crate::pci::all() {
                    let driver = match (class, sub) {
                        (0x02, 0x00) if crate::net::card_name().is_some() && crate::net::drives(vendor, device) => "in use",
                        (0x02, 0x00) if crate::net::drives(vendor, device) => "supported",
                        (0x01, 0x01) | (0x01, 0x06) => "supported",
                        (0x04, 0x01) if (vendor, device) == (0x1274, 0x5000) => "supported",
                        (0x03, _) => "screen via the boot framebuffer",
                        (0x06, _) => "",
                        _ => "no driver",
                    };
                    println!(
                        "{:02x}:{:02x}.{} {:04x}:{:04x} {:<16} {}",
                        d.bus, d.slot, d.function, vendor, device, crate::pci::class_name(class, sub), driver
                    );
                }
            }
            "drives" | "disks" => {
                fs::refresh_disc();
                for d in fs::drives() {
                    let size = if d.bytes > 0 {
                        alloc::format!("{} MB", d.bytes / (1024 * 1024))
                    } else {
                        alloc::string::String::new()
                    };
                    println!("{:<16} {:<10} {:<24} {} {}", d.name, if d.ready { d.path.as_str() } else { "-" }, d.status, size, d.detail);
                }
            }
            "disc" | "cd-rom" => {
                if fs::refresh_disc() {
                    println!("Disc: {}", fs::disc_label().unwrap_or_default());
                    ls(fs::DISC_PATH);
                } else {
                    println!("There is no disc in the drive.");
                }
            }
            "browser" | "web" => {
                if !args.trim().is_empty() {
                    gui::request_address(args.trim());
                }
                open(App::Browser);
            }
            "exit" => {
                if !gui::request_close(App::Terminal) {
                    println!("There is no desktop to go back to in text mode.");
                }
            }
            "fetch" => fetch(args.trim()),
            "vpn" => vpn(args.trim()),
            "js" => js(args.trim()),
            "whoami" => match users::current_name() {
                Some(name) => println!("{}", name.as_str()),
                None => println!("nobody"),
            },
            "users" => {
                for i in 0..users::count() {
                    let password = if users::has_password(i) {
                        "password set"
                    } else {
                        "no password"
                    };
                    if let Some(name) = users::name(i) {
                        println!("{:<16} {}", name.as_str(), password);
                    }
                }
            }
            "useradd" => useradd(args.trim()),
            "passwd" => passwd(args.trim()),
            "lock" => {
                if !gui::request_lock() {
                    println!("There is no lock screen in text mode.");
                }
            }
            "screen" | "resolution" => screen(args.trim()),
            "theme" => match args.trim() {
                "dark" => gui::set_theme(true),
                "light" => gui::set_theme(false),
                _ => println!("usage: theme light | theme dark"),
            },
            "wallpaper" => match args.trim() {
                "" => println!("usage: wallpaper <picture> | wallpaper next"),
                "next" => gui::next_wallpaper(),
                file => {
                    if !gui::set_wallpaper(&self.path(file)) {
                        error("not a PNG, JPEG or BMP picture");
                    }
                }
            },
            "restart" | "reboot" => {
                if !gui::request_power(true) {
                    println!("Restart works on the desktop.");
                }
            }
            "shutdown" | "poweroff" => {
                if !gui::request_power(false) {
                    println!("Shut down works on the desktop.");
                }
            }
            "panic" => match args.trim() {
                "memory" => {
                    // above the 4 GiB the boot code maps: a page fault
                    let bad = 0x0000_1000_0000_0000 as *const u64;
                    let v = unsafe { bad.read_volatile() };
                    println!("{}", v);
                }
                _ => {
                    crate::crash::manual();
                    panic!("panic requested from the shell");
                }
            },
            "crash" => match crate::crash::Report::last() {
                Some(r) => {
                    print!("{}", r.text());
                    open(App::Crash);
                }
                None => println!("No crash report: RyzikOS has not crashed on this disk."),
            },
            _ => console::print_colored(
                Color::LightRed,
                format_args!("unknown command: {} (try 'help')\n", command),
            ),
        }
    }
}

impl Shell {
    /// A path typed in the shell, relative to the current folder.
    /// `zip <archive.zip> <files or folders>`: pack them, adding to the
    /// archive if it is there.
    fn zip(&self, args: &str) {
        let words: alloc::vec::Vec<&str> = args.split_whitespace().collect();
        if words.len() < 2 {
            error("Usage: zip <archive.zip> <files or folders>");
            return;
        }
        let mut target = self.path(words[0]);
        if archive::Archive::kind_of(&target).is_none() {
            target.push_str(".zip");
        }
        let base = match fs::read(&target) {
            Ok(data) => match archive::Archive::parse(data, &target) {
                Ok(a) if a.kind.editable() => a,
                Ok(_) => return error("Only ZIP archives can have files added"),
                Err(e) => return error(&e),
            },
            Err(_) => archive::Archive::new_zip(),
        };
        let mut sources = alloc::vec::Vec::new();
        for w in &words[1..] {
            let path = self.path(w);
            if !fs::exists(&path) {
                return error(&alloc::format!("{}: {}", w, fs::Error::NotFound.message()));
            }
            let name = String::from(fs::file_name(&path));
            sources.push(archive::Source { path, name });
        }
        let result = archive::add(&base, &sources, archive::Level::Normal, &mut |_, _, _| true)
            .and_then(|data| fs::write(&target, &data).map(|()| data.len()).map_err(|e| String::from(e.message())));
        match result {
            Ok(bytes) => {
                println!("Packed {} into {} ({} bytes)", words[1..].join(" "), fs::display(&target), bytes);
                crate::serial::write_str(&alloc::format!("\nzip: packed {} into {}\n", sources.len(), fs::file_name(&target)));
            }
            Err(e) => error(&e),
        }
    }

    /// `unzip <archive> [folder]` unpacks into the folder (by default a
    /// new one named after the archive); `unzip -l <archive>` lists it.
    fn unzip(&self, args: &str) {
        let words: alloc::vec::Vec<&str> = args.split_whitespace().collect();
        let (list, words) = match words.first() {
            Some(&"-l") => (true, &words[1..]),
            _ => (false, &words[..]),
        };
        let Some(first) = words.first() else {
            return error("Usage: unzip <archive> [folder], or unzip -l <archive>");
        };
        let path = self.path(first);
        let a = match fs::read(&path).map_err(|e| String::from(e.message())).and_then(|d| archive::Archive::parse(d, &path)) {
            Ok(a) => a,
            Err(e) => return error(&e),
        };
        if list {
            for e in &a.entries {
                if e.dir {
                    println!("{:>10}  {}/", "", e.name);
                } else {
                    println!("{:>10}  {}", e.size, e.name);
                }
            }
            let (size, _) = a.totals();
            println!("{} entries, {} bytes, {}", a.entries.len(), size, a.kind.name());
            crate::serial::write_str(&alloc::format!("\nunzip: listed {} entries\n", a.entries.len()));
            return;
        }
        let dest = match words.get(1) {
            Some(d) => self.path(d),
            None => {
                let dir = fs::parent(&path);
                let file = fs::file_name(&path);
                let lower = file.to_ascii_lowercase();
                let cut = [".tar.gz", ".tgz", ".zip", ".tar", ".gz"]
                    .iter()
                    .find(|x| lower.ends_with(*x) && file.len() > x.len())
                    .map_or(file.len(), |x| file.len() - x.len());
                let name = fs::unique_name(&dir, &file[..cut], "");
                fs::join(&dir, &name)
            }
        };
        match archive::extract(&a, &[], "", &dest, &mut |_, _, _| true) {
            Ok(n) => {
                println!("Unpacked {} {} into {}", n, if n == 1 { "file" } else { "files" }, fs::display(&dest));
                crate::serial::write_str(&alloc::format!("\nunzip: unpacked {} files to {}\n", n, fs::display(&dest)));
            }
            Err(e) => error(&e),
        }
    }

    fn path(&self, arg: &str) -> String {
        let arg = arg.trim();
        let cwd = if self.cwd.is_empty() {
            fs::home(users::current_name().unwrap_or_default().as_str())
        } else {
            self.cwd.clone()
        };
        if arg.starts_with(['/', '\\']) || arg.starts_with("C:") || arg.starts_with("c:") {
            fs::parse(arg)
        } else {
            fs::parse(&fs::join(&cwd, arg))
        }
    }
}

fn report(result: Result<(), fs::Error>) {
    if let Err(e) = result {
        error(e.message());
    }
}

/// Print a folder like `dir` on Windows.
fn ls(path: &str) {
    match fs::list(path) {
        Ok(items) => {
            // on one line first, so the boot test can find it
            println!("ls: {} ({} items)", fs::display(path), items.len());
            for item in items {
                let (y, mo, d, h, mi) = item.modified;
                if item.dir {
                    print!("{:02}.{:02}.{} {:02}:{:02}  <DIR>      ", d, mo, y, h, mi);
                } else {
                    print!(
                        "{:02}.{:02}.{} {:02}:{:02}  {:>10} ",
                        d, mo, y, h, mi, item.size
                    );
                }
                println!("{}", item.name);
            }
        }
        Err(e) => error(e.message()),
    }
}

fn error(message: &str) {
    console::print_colored(Color::LightRed, format_args!("{}\n", message));
}

fn is_root() -> bool {
    users::current_name().is_some_and(|name| name.as_str() == "root")
}

fn useradd(args: &str) {
    let mut parts = args.split_whitespace();
    let (Some(name), password) = (parts.next(), parts.next()) else {
        println!("usage: useradd <name> [password]");
        return;
    };
    if !is_root() {
        error("only root can add users");
        return;
    }
    match users::add(name, password.unwrap_or("")) {
        Ok(()) => println!("added {}; they can sign in on the lock screen", name),
        Err(e) => error(e.message()),
    }
}

fn passwd(args: &str) {
    let mut parts = args.split_whitespace();
    let (name, password) = match (parts.next(), parts.next()) {
        (Some(name), Some(password)) => (Some(name), password),
        (Some(password), None) => (None, password),
        _ => {
            println!("usage: passwd [<name>] <password>");
            return;
        }
    };
    let me = users::current_name().unwrap_or_default();
    let name = name.unwrap_or(me.as_str());
    if name != me.as_str() && !is_root() {
        error("only root can change other users' passwords");
        return;
    }
    match users::set_password(name, password) {
        Ok(()) => println!("password changed for {}", name),
        Err(e) => error(e.message()),
    }
}

fn open(app: App) {
    if !gui::request_open(app) {
        println!("No graphics: GRUB started RyzikOS in text mode.");
    }
}

fn info(boot: &BootInfo) {
    let (cols, rows) = CONSOLE.lock().size();
    match &boot.framebuffer {
        Some(fb) => println!(
            "Screen:     {}x{}, {} bits per pixel, {}x{} characters",
            fb.width,
            fb.height,
            fb.bytes_per_pixel * 8,
            cols,
            rows
        ),
        None => println!("Screen:     VGA text mode, {}x{} characters", cols, rows),
    }
    println!(
        "Memory:     {} MiB above 1 MiB",
        boot.upper_memory_kib / 1024
    );
    println!("Bootloader: {}", boot.bootloader);
    let seconds = interrupts::ticks() / interrupts::TIMER_HZ;
    println!(
        "Uptime:     {}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
}

fn colors() {
    const NAMES: [(Color, &str); 16] = [
        (Color::Black, "black"),
        (Color::Blue, "blue"),
        (Color::Green, "green"),
        (Color::Cyan, "cyan"),
        (Color::Red, "red"),
        (Color::Magenta, "magenta"),
        (Color::Brown, "brown"),
        (Color::LightGray, "light gray"),
        (Color::DarkGray, "dark gray"),
        (Color::LightBlue, "light blue"),
        (Color::LightGreen, "light green"),
        (Color::LightCyan, "light cyan"),
        (Color::LightRed, "light red"),
        (Color::Pink, "pink"),
        (Color::Yellow, "yellow"),
        (Color::White, "white"),
    ];
    for (i, (color, name)) in NAMES.iter().enumerate() {
        CONSOLE.lock().set_color(Color::Black, *color);
        print!("    ");
        // black text would be invisible, so name it in dark gray
        let label = if *color == Color::Black {
            Color::DarkGray
        } else {
            *color
        };
        CONSOLE.lock().set_color(label, Color::Black);
        print!(" {:<12}", name);
        CONSOLE.lock().set_color(Color::LightGray, Color::Black);
        if i % 4 == 3 {
            println!();
        }
    }
}

/// Download a page in the terminal: the network test without the browser.
/// `vpn add <subscription or link>`, `vpn list`, `vpn on [n]`, `vpn off`,
/// `vpn test [n]`, `vpn log`; plain `vpn` opens the app.
fn vpn(args: &str) {
    use crate::vpn;
    let (command, rest) = args.split_once(' ').unwrap_or((args, ""));
    let rest = rest.trim();
    if !matches!(command, "" | "help" | "log" | "off" | "list" | "probe") && crate::net::init().is_none() {
        println!("{}", crate::net::NO_CARD);
        return;
    }
    let pick = |saved: &vpn::Saved| -> Option<vpn::Server> {
        let all: alloc::vec::Vec<&vpn::Server> = saved.groups.iter().flat_map(|g| &g.servers).collect();
        match rest.parse::<usize>() {
            Ok(n) if n >= 1 => all.get(n - 1).map(|s| (*s).clone()),
            _ => all
                .iter()
                .find(|s| s.link == saved.selected)
                .or(all.first())
                .map(|s| (*s).clone()),
        }
    };
    match command {
        "" => open(App::Vpn),
        "add" if !rest.is_empty() => {
            let mut saved = vpn::load();
            let group = if rest.contains("://") && !rest.starts_with("http") && !rest.starts_with("happ://") {
                let servers = vpn::link::parse_list(rest);
                if servers.is_empty() {
                    error("Not a link RyzikOS knows (vless://, trojan://, ss://)");
                    return;
                }
                vpn::Group { url: String::new(), title: String::new(), usage: None, servers }
            } else {
                match vpn::fetch_subscription(rest) {
                    Ok(g) => g,
                    Err(e) => {
                        error(&e);
                        return;
                    }
                }
            };
            println!("Added {} server(s).", group.servers.len());
            vpn::add_group(&mut saved, group);
            if let Err(e) = vpn::save(&saved) {
                error(&e);
            }
        }
        "list" => {
            let saved = vpn::load();
            let mut n = 0;
            for g in &saved.groups {
                println!("{}", if g.url.is_empty() { "Added by hand" } else { g.title.as_str() });
                for s in &g.servers {
                    n += 1;
                    let mark = if s.link == saved.selected { "*" } else { " " };
                    let why = s.unsupported().map(|w| alloc::format!(" ({})", w)).unwrap_or_default();
                    println!("{} {:>2}. {} [{}]{}", mark, n, s.name, s.protocol(), why);
                }
            }
            if n == 0 {
                println!("No servers. Add one: vpn add <subscription address or vless:// link>");
            }
        }
        "on" | "connect" | "test" => {
            let mut saved = vpn::load();
            let Some(server) = pick(&saved) else {
                error("No such server; see vpn list");
                return;
            };
            println!("{} {} ...", if command == "test" { "Testing" } else { "Connecting to" }, server.name);
            let result = if command == "test" { vpn::test(&server) } else { vpn::connect(&server) };
            match result {
                Ok(ms) => {
                    println!("{} ({} ms)", if command == "test" { "Works" } else { "VPN is on" }, ms);
                    crate::serial::write_str(&alloc::format!("\nvpn: {} ok {} ms\n", command, ms));
                    if command != "test" {
                        saved.selected = server.link.clone();
                        let _ = vpn::save(&saved);
                    }
                }
                Err(e) => {
                    crate::serial::write_str(&alloc::format!("\nvpn: {} failed: {}\n", command, e));
                    error(&e);
                }
            }
        }
        "off" => {
            vpn::disconnect();
            println!("VPN is off.");
        }
        "status" => match vpn::route() {
            Some(s) => {
                let (up, down) = vpn::traffic();
                println!("On: {} [{}], {} s, sent {}, received {}", s.name, s.protocol(), vpn::uptime(), vpn::size_text(up), vpn::size_text(down));
            }
            None => println!("Off."),
        },
        "probe" if !rest.is_empty() => vpn::set_probe(rest),
        "log" => {
            for l in vpn::recent_log() {
                println!("{}", l);
            }
        }
        _ => {
            println!("vpn                open the VPN app");
            println!("vpn add <address>  add a subscription, or a vless://, trojan:// or ss:// link");
            println!("vpn list           list the servers (* is the chosen one)");
            println!("vpn on [n]         connect through server n (vpn off disconnects)");
            println!("vpn test [n]       measure the delay through server n");
            println!("vpn status, vpn log");
        }
    }
}

fn fetch(address: &str) {
    if address.is_empty() {
        println!("usage: fetch <address>");
        return;
    }
    if crate::net::init().is_none() {
        println!("{}", crate::net::NO_CARD);
        return;
    }
    let Some(url) = crate::web::address_to_url(address) else {
        println!("bad address: {}", address);
        return;
    };
    println!("Loading {} ...", url);
    let page = crate::web::load(&url, None, (1200, 800), true);
    let dom = &page.dom;
    let text = dom
        .text_content(dom.body())
        .split_whitespace()
        .map(|w| w.chars().count())
        .sum::<usize>();
    let links: alloc::vec::Vec<alloc::string::String> = dom
        .descendants(crate::web::dom::DOCUMENT)
        .into_iter()
        .filter(|&n| dom.tag(n) == "a")
        .filter_map(|n| dom.attr(n, "href").map(alloc::string::ToString::to_string))
        .collect();
    // on one line, so the boot test can find it on the serial port
    println!(
        "fetch: \"{}\", {} characters of text, {} links, page height {} px",
        page.title(),
        text,
        links.len(),
        page.layout.height
    );
    for link in links.iter().take(8) {
        println!("  {}", link);
    }
}

/// Evaluate a line of JavaScript and print the result.
fn js(source: &str) {
    if source.is_empty() {
        println!("usage: js <expression>, e.g. js [1, 2, 3].map(x => x * 2)");
        return;
    }
    let Some(mut ctx) = crate::js::Context::new() else {
        println!("js: out of memory");
        return;
    };
    let mut host = crate::js::ConsoleHost;
    let _ = ctx.eval(&mut host, crate::js::BASE_PRELUDE, "<prelude>");
    let t0 = crate::js::now_ms();
    match ctx.eval(&mut host, source, "<shell>") {
        Ok(v) => println!("{}", v),
        Err(e) => println!("error: {}", e),
    }
    crate::serial::write_str(&alloc::format!(
        "\njs: done in {} ms\n",
        crate::js::now_ms() - t0
    ));
}

/// `screen`: the resolution now and the ones to pick, or `screen WxH`
/// to switch (in virtual machines whose graphics card allows it).
fn screen(args: &str) {
    let Some(fb) = CONSOLE.lock().framebuffer() else {
        return error("No graphics screen.");
    };
    let mode = args
        .split_once('x')
        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)));
    match mode {
        Some(m) if crate::display::MODES.contains(&m) => {
            if !crate::display::can_change(&fb) {
                return error("This graphics card keeps the resolution the computer started with.");
            }
            crate::gui::request_screen(m.0, m.1);
        }
        _ if !args.is_empty() => error("Pick one of the resolutions 'screen' lists, like screen 1920x1200"),
        _ => {
            println!("Screen: {} x {}", fb.width, fb.height);
            if crate::display::can_change(&fb) {
                let list: alloc::vec::Vec<alloc::string::String> = crate::display::MODES
                    .iter()
                    .map(|(w, h)| alloc::format!("{}x{}", w, h))
                    .collect();
                println!("Can switch to: {}", list.join(", "));
            } else {
                println!("This graphics card keeps the resolution the computer started with.");
            }
        }
    }
}
