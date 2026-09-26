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
                return message_page(
                    Some(resp.url),
                    "Этот файл не показать",
                    &format!("Сервер прислал файл типа {} ({} байт). EverBrowser показывает веб-страницы, текст и картинки PNG и JPEG.", ct, resp.body.len()),
                    viewport,
                );
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

const HOME_HTML: &str = r#"<!doctype html><title>EverBrowser</title>
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
  <h1>EverBrowser</h1>
  <p>Браузер RyzikOS: свой HTML, CSS и JavaScript (QuickJS), написанный с нуля на Rust.</p>
  <form action="https://html.duckduckgo.com/html/"><input type="text" name="q" placeholder="Поиск в DuckDuckGo"><input type="submit" value="Найти"></form>
</div>
<main>
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
