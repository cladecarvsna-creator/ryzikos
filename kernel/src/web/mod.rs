//! The web engine behind the browser: addresses, HTTP(S), HTML into a
//! DOM, CSS, layout, images and JavaScript. Drawing and input live in
//! `gui::browser`.

pub mod css;
pub mod dom;
pub mod http;
pub mod image;
pub mod layout;
pub mod page;
pub mod style;
pub mod text;
pub mod url;

use alloc::format;
use alloc::string::{String, ToString};

pub use page::Page;
use url::Url;

pub const HOME: &str = "about:home";

/// Where the browser should go.
#[derive(Clone)]
pub enum Nav {
    Home,
    Get(Url),
    Post(Url, String),
    /// A page made by RyzikOS itself (`about:programs`,
    /// `about:downloads`), a file on the disk (`file:/Users/...`) or a
    /// request to the desktop (`ryzikos:open:<path>`).
    Special(String),
}

/// Whether an address is one of RyzikOS's own (see `Nav::Special`).
pub fn is_special(address: &str) -> bool {
    let a = address.trim().to_ascii_lowercase();
    (a.starts_with("about:") && a != HOME) || a.starts_with("file:") || a.starts_with("ryzikos:")
}

/// Programs downloaded from the catalog: web apps, one HTML file each.
pub const PROGRAM_EXT: &str = ".rzapp";
/// Where the catalog and its programs are downloaded from: the
/// `programs` folder of the RyzikOS repository.
/// A build can point it elsewhere with RYZIKOS_CATALOG (tests use a
/// local server).
pub const CATALOG_BASE: &str = match option_env!("RYZIKOS_CATALOG") {
    Some(url) => url,
    None => "https://raw.githubusercontent.com/cladecarvsna-creator/ryzikos/main/programs/",
};
/// The catalog as it was when this RyzikOS was built, for when there is
/// no internet. The App Store downloads the newest one.
pub const CATALOG_TEXT: &str = include_str!("../../../programs/catalog.txt");

/// A program in the catalog.
#[derive(Clone)]
pub struct CatalogEntry {
    pub file: String,
    pub name: String,
    pub category: String,
    pub about: String,
}

/// Read a catalog: one program a line, `file | name | category | about`,
/// with `#` comments. Lines that don't name a program file are skipped.
pub fn parse_catalog(text: &str) -> alloc::vec::Vec<CatalogEntry> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.split('|').map(str::trim);
            let file = parts.next()?;
            let safe = file.ends_with(PROGRAM_EXT)
                && !file.contains('/')
                && !file.contains('\\')
                && crate::fs::valid_name(file);
            if !safe {
                return None;
            }
            Some(CatalogEntry {
                file: String::from(file),
                name: String::from(parts.next().filter(|n| !n.is_empty()).unwrap_or(file.trim_end_matches(PROGRAM_EXT))),
                category: String::from(parts.next().unwrap_or("Tools")),
                about: String::from(parts.next().unwrap_or("")),
            })
        })
        .collect()
}

/// Turn what was typed in the address bar into an address: a URL, or a
/// search for anything that does not look like one.
pub fn address_to_url(text: &str) -> Option<Url> {
    let text = text.trim();
    let looks_like_url = text.contains("://")
        || (!text.contains(' ') && (text.contains('.') || text.starts_with("localhost")));
    if looks_like_url {
        if let Some(u) = Url::parse(text) {
            return Some(u);
        }
    }
    Url::parse(&format!(
        "https://html.duckduckgo.com/html/?q={}",
        url::encode_query(text)
    ))
}

/// Download a page, run its scripts and lay it out. Never fails: errors
/// become an error page.
pub fn load(url: &Url, form: Option<&String>, viewport: (i32, i32), scripts: bool) -> Page {
    match http::get(url, form.map(|f| f.as_str())) {
        Ok(resp) => {
            let ct = resp.content_type.clone();
            if wants_download(&resp) {
                return save_download(resp, viewport);
            }
            if ct.starts_with("image/") {
                let mut page = from_html(
                    Some(resp.url.clone()),
                    &format!("<title>{0}</title><body style=\"margin:0;background:#202124;text-align:center\"><img src=\"{0}\" style=\"max-width:100%\">", escape(&resp.url.to_string())),
                    viewport,
                );
                if let Some(img) = image::decode(&resp.body) {
                    page.images.insert(resp.url.to_string(), img);
                }
                return page;
            }
            let is_text = ct.is_empty() || ct.starts_with("text/") || ct.contains("html") || ct.contains("xml") || ct.contains("json") || ct.contains("javascript");
            if !is_text {
                return save_download(resp, viewport);
            }
            let source = text::decode(&resp.body, &ct);
            let html = ct.is_empty() || ct.contains("html") || ct.contains("xml") && source.trim_start().starts_with('<');
            let source = if html {
                source
            } else {
                format!("<pre style=\"white-space:pre-wrap\">{}</pre>", escape(&source))
            };
            let mut page = Page::new(Some(resp.url), dom::parse(&source), viewport);
            if scripts {
                page.run_scripts();
            }
            page.update();
            crate::serial::write_str("\nbrowser: loaded page\n");
            page
        }
        Err(e) => message_page(
            Some(url.clone()),
            "Не удаётся открыть страницу",
            &format!(
                "{}: {}. Проверьте, что QEMU запущен с сетью (-nic user,model=e1000) и у компьютера есть интернет.",
                url.host, e
            ),
            viewport,
        ),
    }
}

/// Whether a reply is a file to keep rather than a page to show: the
/// server says so, or the name says it is a program, a video or an
/// archive.
fn wants_download(resp: &http::Response) -> bool {
    if resp.disposition.to_ascii_lowercase().starts_with("attachment") {
        return true;
    }
    let name = url::decode_percent(resp.url.path.split('?').next().unwrap_or("")).to_ascii_lowercase();
    [
        PROGRAM_EXT, ".avi", ".mjpg", ".mjpeg", ".zip", ".iso", ".img", ".exe", ".bin", ".mp4",
        ".mp3", ".wav", ".pdf", ".7z", ".gz", ".tar",
    ]
    .iter()
    .any(|ext| name.ends_with(ext))
}

/// The file name for a download: from Content-Disposition, else from the
/// end of the address.
fn download_name(resp: &http::Response) -> String {
    let from_header = resp
        .disposition
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("filename="))
        .next()
        .map(|n| n.trim_matches('"').to_string());
    let from_path = || {
        let path = resp.url.path.split('?').next().unwrap_or("");
        url::decode_percent(path.rsplit('/').next().unwrap_or(""))
    };
    let raw = from_header.unwrap_or_else(from_path);
    let clean: String = raw
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        String::from("download")
    } else {
        clean
    }
}

/// Keep a downloaded file: programs go to Programs, everything else to
/// Downloads. Returns the page that says where it went.
fn save_download(resp: http::Response, viewport: (i32, i32)) -> Page {
    use crate::fs;
    let user = crate::users::current_name().unwrap_or_default();
    let name = download_name(&resp);
    let program = name.to_ascii_lowercase().ends_with(PROGRAM_EXT);
    let dir = fs::join(&fs::home(user.as_str()), if program { "Programs" } else { "Downloads" });
    let _ = fs::create_dir(&dir);
    // a program is replaced by its new version; other files get a new name
    let name = if program || !fs::exists(&fs::join(&dir, &name)) {
        name
    } else {
        let (base, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name.as_str(), ""),
        };
        fs::unique_name(&dir, base, ext)
    };
    let path = fs::join(&dir, &name);
    let size = resp.body.len();
    crate::serial::write_str(&format!("\nbrowser: downloaded {} ({} bytes)\n", name, size));
    if let Err(e) = fs::write(&path, &resp.body) {
        return message_page(
            Some(resp.url),
            "Download failed",
            &format!("{} could not be saved: {}", name, e.message()),
            viewport,
        );
    }
    let size_text = if size >= 1024 * 1024 {
        format!("{}.{} MB", size / (1024 * 1024), size % (1024 * 1024) * 10 / (1024 * 1024))
    } else {
        format!("{} KB", size.div_ceil(1024))
    };
    let (title, text, action) = if program {
        (
            format!("{} is installed", escape(name.trim_end_matches(PROGRAM_EXT))),
            format!("The program is in {}. It is also in the launcher's Programs list and on the Programs page.", escape(&fs::display(&dir))),
            "Run it",
        )
    } else {
        (
            format!("Downloaded {}", escape(&name)),
            format!("{}, saved in {}.", size_text, escape(&fs::display(&dir))),
            "Open",
        )
    };
    let source = format!(
        "<title>{title}</title>{STYLE}<div class=box><div class=ok>&#10003;</div><h1>{title}</h1><p>{text}</p>\
         <p><a class=btn href=\"ryzikos:open:{path}\">{action}</a> <a class=btn2 href=\"ryzikos:folder:{dir}\">Show in Files</a> <a class=btn2 href=\"about:downloads\">All downloads</a></p></div>",
        title = title,
        text = text,
        path = escape(&path),
        dir = escape(&dir),
        action = action,
        STYLE = SPECIAL_STYLE,
    );
    from_html(Some(resp.url), &source, viewport)
}

const SPECIAL_STYLE: &str = "<style>body{font-family:sans-serif;margin:0;background:#f4f6fb;color:#1d2230}\
.box{max-width:760px;margin:48px auto;background:white;border:1px solid #e1e5ee;border-radius:16px;padding:28px 36px}\
h1{font-size:26px;margin:6px 0 10px}h2{font-size:19px;margin:26px 0 12px}p{line-height:1.5;color:#4a5263}\
.ok{width:48px;height:48px;border-radius:24px;background:#1f9d55;color:white;font-size:30px;text-align:center;line-height:48px}\
a.btn{background:#2f6fed;color:white;padding:9px 20px;border-radius:18px;text-decoration:none;font-weight:bold}\
a.btn2{background:#e8edf8;color:#1d3f8f;padding:9px 16px;border-radius:18px;text-decoration:none;margin-left:6px}\
.row{display:flex;align-items:center;gap:14px;border-top:1px solid #edf0f5;padding:12px 0}\
.row div.t{flex:1}.row b{font-size:16px}.row span{color:#6a7385;font-size:14px}\
.tile{width:44px;height:44px;border-radius:10px;background:linear-gradient(135deg,#2f6fed,#8a4df0);color:white;font-weight:bold;font-size:20px;text-align:center;line-height:44px}\
</style>";

/// One of RyzikOS's own pages. `ryzikos:` requests are handled by the
/// browser before they get here.
pub fn special(address: &str, viewport: (i32, i32)) -> Page {
    let a = address.trim();
    let lower = a.to_ascii_lowercase();
    if let Some(path) = a.strip_prefix("file:").or_else(|| a.strip_prefix("FILE:")) {
        let path = path.trim_start_matches("//");
        return match crate::fs::read(path) {
            Ok(data) => {
                let source = text::decode(&data, "text/html");
                let mut page = Page::new(None, dom::parse(&source), viewport);
                page.run_scripts();
                page.update();
                crate::serial::write_str("\nbrowser: opened a file from the disk\n");
                page
            }
            Err(e) => message_page(None, "Can't open this file", &format!("{}: {}", path, e.message()), viewport),
        };
    }
    if lower == "about:programs" {
        return from_html(None, &programs_page(), viewport);
    }
    if lower == "about:downloads" {
        return from_html(None, &downloads_page(), viewport);
    }
    message_page(None, "Unknown page", &format!("RyzikOS has no page called {}.", a), viewport)
}

fn programs_in(dir: &str) -> alloc::vec::Vec<String> {
    crate::fs::list(dir)
        .map(|items| {
            items
                .into_iter()
                .filter(|i| !i.dir && i.name.to_ascii_lowercase().ends_with(PROGRAM_EXT))
                .map(|i| i.name)
                .collect()
        })
        .unwrap_or_default()
}

/// The folder a user's programs are installed in.
pub fn programs_folder() -> String {
    let user = crate::users::current_name().unwrap_or_default();
    crate::fs::join(&crate::fs::home(user.as_str()), "Programs")
}

/// Programs the user has installed, as paths.
pub fn installed_programs() -> alloc::vec::Vec<String> {
    let dir = programs_folder();
    programs_in(&dir).into_iter().map(|n| crate::fs::join(&dir, &n)).collect()
}

fn programs_page() -> String {
    use core::fmt::Write;
    let dir = programs_folder();
    let installed = programs_in(&dir);
    let mut h = String::new();
    let _ = write!(h, "<title>Programs</title>{}<div class=box><h1>Programs for RyzikOS</h1>\
        <p>Download a program and it is installed in your Programs folder. Programs are small web apps that run in the browser, without the internet once they are downloaded.</p>", SPECIAL_STYLE);
    if !installed.is_empty() {
        h.push_str("<h2>Installed</h2>");
        for name in &installed {
            let title = name.trim_end_matches(PROGRAM_EXT);
            let _ = write!(
                h,
                "<div class=row><div class=tile>{}</div><div class=t><b>{}</b></div><a class=btn href=\"ryzikos:open:{}\">Run</a></div>",
                escape(&title.chars().next().unwrap_or('?').to_string()),
                escape(title),
                escape(&crate::fs::join(&dir, name))
            );
        }
    }
    h.push_str("<h2>Get programs</h2>");
    for entry in parse_catalog(CATALOG_TEXT) {
        let (file, title, about) = (entry.file.as_str(), entry.name.as_str(), entry.about.as_str());
        let have = installed.iter().any(|n| n.eq_ignore_ascii_case(file));
        let _ = write!(
            h,
            "<div class=row><div class=tile>{}</div><div class=t><b>{}</b><br><span>{}</span></div><a class=btn href=\"{}{}\">{}</a></div>",
            escape(&title.chars().next().unwrap_or('?').to_string()),
            escape(title),
            escape(about),
            CATALOG_BASE,
            file,
            if have { "Update" } else { "Download" }
        );
    }
    let disc = format!("{}/Programs", crate::fs::DISC_PATH);
    let on_disc = programs_in(&disc);
    if !on_disc.is_empty() {
        h.push_str("<h2>On the RyzikOS disc</h2><p>No internet? Install them from the disc in the drive.</p>");
        for name in &on_disc {
            let _ = write!(
                h,
                "<div class=row><div class=tile>{}</div><div class=t><b>{}</b></div><a class=btn2 href=\"ryzikos:install:{}\">Install from disc</a></div>",
                escape(&name.chars().next().unwrap_or('?').to_string()),
                escape(name.trim_end_matches(PROGRAM_EXT)),
                escape(&crate::fs::join(&disc, name))
            );
        }
    }
    h.push_str("<p><a class=btn2 href=\"about:downloads\">Downloads</a> <a class=btn2 href=\"about:home\">Home page</a></p></div>");
    h
}

fn downloads_page() -> String {
    use core::fmt::Write;
    let user = crate::users::current_name().unwrap_or_default();
    let dir = crate::fs::join(&crate::fs::home(user.as_str()), "Downloads");
    let items = crate::fs::list(&dir).unwrap_or_default();
    let mut h = String::new();
    let _ = write!(h, "<title>Downloads</title>{}<div class=box><h1>Downloads</h1><p>Files the browser downloaded are kept in {}. Programs go to the Programs page.</p>", SPECIAL_STYLE, escape(&crate::fs::display(&dir)));
    let files: alloc::vec::Vec<_> = items.into_iter().filter(|i| !i.dir).collect();
    if files.is_empty() {
        h.push_str("<p><i>Nothing downloaded yet.</i></p>");
    }
    for f in files {
        let _ = write!(
            h,
            "<div class=row><div class=t><b>{}</b><br><span>{} KB</span></div><a class=btn href=\"ryzikos:open:{}\">Open</a></div>",
            escape(&f.name),
            (f.size as usize).div_ceil(1024),
            escape(&crate::fs::join(&dir, &f.name))
        );
    }
    let _ = write!(h, "<p><a class=btn2 href=\"ryzikos:folder:{}\">Show in Files</a> <a class=btn2 href=\"about:programs\">Programs</a></p></div>", escape(&dir));
    h
}

fn from_html(url: Option<Url>, source: &str, viewport: (i32, i32)) -> Page {
    let mut page = Page::new(url, dom::parse(source), viewport);
    page.update();
    page
}

fn message_page(url: Option<Url>, title: &str, text: &str, viewport: (i32, i32)) -> Page {
    let source = format!(
        "<title>{0}</title><style>body{{font-family:sans-serif;margin:60px auto;max-width:720px;color:#202124}}h1{{font-size:28px;font-weight:normal}}p{{line-height:1.5;color:#5f6368}}a{{color:#1a73e8}}</style><h1>{0}</h1><p>{1}</p><p><a href=\"about:home\">Домашняя страница</a></p>",
        title,
        escape(text)
    );
    from_html(url, &source, viewport)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The start page.
pub fn home(viewport: (i32, i32)) -> Page {
    let mut page = Page::new(None, dom::parse(HOME_HTML), viewport);
    page.run_scripts();
    page.update();
    page
}

const HOME_HTML: &str = r#"<!doctype html><title>RyzikOS Browser</title>
<style>
body { margin: 0; font-family: sans-serif; color: #202124; background: #f6f8fc; }
.hero { background: linear-gradient(135deg, #1a73e8, #6c3fd1); color: white; padding: 56px 24px 64px; text-align: center; }
.hero h1 { font-size: 44px; margin: 0 0 8px; font-weight: bold; }
.hero p { font-size: 17px; margin: 0 0 28px; color: #e8eefc; }
form { display: flex; justify-content: center; gap: 8px; }
input[type=text] { width: 460px; font-size: 17px; padding: 10px 18px; border: 0; border-radius: 24px; }
input[type=submit] { font-size: 16px; padding: 10px 22px; border: 0; border-radius: 24px; background: #ffffff; color: #1a73e8; font-weight: bold; }
main { max-width: 980px; margin: 0 auto; padding: 32px 24px; }
h2 { font-size: 20px; margin: 8px 0 16px; }
.grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px; }
.card { background: white; border: 1px solid #e3e7ef; border-radius: 12px; padding: 16px 18px; }
.card a { font-size: 17px; font-weight: bold; color: #1a73e8; text-decoration: none; }
.card p { margin: 6px 0 0; color: #5f6368; font-size: 14px; }
.note { margin-top: 28px; padding: 14px 18px; border-left: 4px solid #1a73e8; background: white; border-radius: 8px; color: #3c4043; font-size: 14px; line-height: 1.5; }
#clock { font-weight: bold; color: #1a73e8; }
</style>
<div class="hero">
  <h1>RyzikOS Browser</h1>
  <p>Браузер RyzikOS: свой HTML, CSS и JavaScript (QuickJS), написанный с нуля на Rust.</p>
  <form action="https://html.duckduckgo.com/html/"><input type="text" name="q" placeholder="Поиск в DuckDuckGo"><input type="submit" value="Найти"></form>
</div>
<main>
  <div class="card" style="margin-bottom:24px;border-color:#b9ccf7"><a href="about:programs">Программы для RyzikOS</a><p>Скачайте игры и утилиты: они установятся в папку Programs и появятся в лаунчере. Все скачанные файлы: <a href="about:downloads" style="font-size:14px">about:downloads</a></p></div>
  <h2>Попробуйте эти сайты</h2>
  <div class="grid">
    <div class="card"><a href="http://example.com/">example.com</a><p>Классическая страница-пример</p></div>
    <div class="card"><a href="https://en.wikipedia.org/wiki/Operating_system">Wikipedia</a><p>Статья про операционные системы</p></div>
    <div class="card"><a href="https://ru.wikipedia.org/wiki/Операционная_система">Википедия</a><p>То же по-русски</p></div>
    <div class="card"><a href="http://info.cern.ch/hypertext/WWW/TheProject.html">info.cern.ch</a><p>Самый первый сайт в мире</p></div>
    <div class="card"><a href="https://news.ycombinator.com/">Hacker News</a><p>Новости для программистов</p></div>
    <div class="card"><a href="https://lite.duckduckgo.com/lite/">DuckDuckGo Lite</a><p>Поиск без лишнего</p></div>
    <div class="card"><a href="https://text.npr.org/">NPR</a><p>Новости текстом</p></div>
    <div class="card"><a href="http://68k.news/">68k.news</a><p>Новости для старых компьютеров</p></div>
    <div class="card"><a href="http://frogfind.com/">FrogFind</a><p>Упрощает любые сайты</p></div>
  </div>
  <div class="note">Сейчас <span id="clock">...</span>. Эти часы идут благодаря JavaScript на этой странице.
  Колёсико мыши, Page Up и Page Down прокручивают страницу, Ctrl+L переходит в адресную строку.
  HTTPS шифрует соединение, но сертификаты сайтов не проверяются.</div>
</main>
<script>
const clock = document.getElementById('clock');
const two = n => String(n).padStart(2, '0');
const tick = () => { const d = new Date(); clock.textContent = two(d.getHours()) + ':' + two(d.getMinutes()) + ':' + two(d.getSeconds()); };
tick();
setInterval(tick, 1000);
</script>
"#;
