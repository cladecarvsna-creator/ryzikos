import { Clock, Reveal, Typing } from "../components/Live";
import { latestRelease, REPO_URL, RELEASES_URL } from "../lib/release";

// look for a new build every two minutes
export const revalidate = 120;

const FEATURES = [
  { icon: "explorer", title: "Файлы", text: "Все диски IDE и SATA, CD и DVD, FAT32 с длинными именами, корзина и поиск." },
  { icon: "store", title: "App Store", text: "Игры и инструменты в один клик: ставятся из интернета или с диска RyzikOS." },
  { icon: "browser", title: "Браузер", text: "Вкладки, HTTPS, CSS с flexbox и grid, JavaScript на QuickJS. Открывает Википедию." },
  { icon: "photos", title: "Фото", text: "Галерея картинок PNG и JPEG из папок, дисков и CD, с масштабом и поворотом." },
  { icon: "video", title: "Видео", text: "Видеоплеер MJPEG AVI с перемоткой и повтором." },
  { icon: "taskmgr", title: "Диспетчер задач", text: "Открытые программы, «Завершить задачу», графики процессора и памяти." },
  { icon: "terminal", title: "Терминал", text: "Командная строка: файлы, сеть, диски, команда devices со списком железа." },
  { icon: "settings", title: "Настройки", text: "Тёмная и светлая тема, обои, пользователи, сеть, язык ENG и РУС." },
  { icon: "program", title: "Свои программы", text: "Программы .rzapp — HTML и JavaScript в одном файле, в собственном окне." },
];

const DOCK = [
  { href: "#top", icon: "/logo.png", tip: "RyzikOS" },
  { href: "#features", icon: "/icons/explorer.png", tip: "Возможности" },
  { href: "#screens", icon: "/icons/photos.png", tip: "Скриншоты" },
  { href: "#install", icon: "/icons/terminal.png", tip: "Установка", sm: true },
  { href: "#download", icon: "/icons/store.png", tip: "Скачать" },
  { sep: true },
  { href: REPO_URL, icon: "/icons/browser.png", tip: "GitHub", external: true },
  { href: "/download", icon: "/icons/desktops.png", tip: "Скачать ISO", sm: true },
];

const SHOTS = [
  { file: "desktop-icons", title: "Рабочий стол", icon: "computer", wide: true },
  { file: "app-store", title: "App Store", icon: "store" },
  { file: "task-manager", title: "Task Manager", icon: "taskmgr" },
  { file: "files-computer", title: "Files", icon: "explorer" },
  { file: "program-window", title: "Snake", icon: "program" },
  { file: "photos-gallery", title: "Photos", icon: "photos" },
  { file: "launcher", title: "Launcher", icon: "search" },
];

const TERMINAL = [
  { cls: "p", text: "ryzikos> " },
  { cls: "c", text: "devices\n" },
  { cls: "m", text: "00:03.0 8086:10d3 Ethernet         in use\n00:04.0 1274:5000 sound card       supported\n" },
  { cls: "p", text: "ryzikos> " },
  { cls: "c", text: "fetch http://example.com\n" },
  { cls: "m", text: "fetch: \"Example Domain\", 1 links\n" },
  { cls: "p", text: "ryzikos> " },
  { cls: "c", text: "open ~/Programs/snake.rzapp\n" },
  { cls: "p", text: "ryzikos> " },
];

function Window({ title, icon, dark, children, className = "" }) {
  return (
    <div className={`window ${dark ? "dark" : ""} ${className}`}>
      <div className="titlebar">
        <div className="lights">
          <i />
          <i />
          <i />
        </div>
        <div className="title">
          {icon && <img src={icon} alt="" />}
          {title}
        </div>
      </div>
      {children}
    </div>
  );
}

function DownloadIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 4v11M7 10l5 5 5-5M5 20h14" />
    </svg>
  );
}

function formatSize(bytes) {
  return `${(bytes / (1024 * 1024)).toFixed(1)} МБ`;
}

function formatDate(iso) {
  return new Date(iso).toLocaleDateString("ru-RU", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
}

export default async function Home() {
  const release = await latestRelease();

  return (
    <>
      <div className="wallpaper" />
      <Reveal />

      <header className="menubar">
        <a className="brand" href="#top">
          <img src="/logo.png" alt="" />
          RyzikOS
        </a>
        <nav>
          <a href="#features">Возможности</a>
          <a href="#screens">Скриншоты</a>
          <a href="#install">Установка</a>
          <a href="#download">Скачать</a>
          <a href={REPO_URL}>GitHub</a>
        </nav>
        <div className="tray">
          <span className="dim hide-sm">ENG</span>
          <Clock />
        </div>
      </header>

      <main id="top">
        <section className="hero">
          <div>
            <img className="logo" src="/logo.png" alt="Логотип RyzikOS" />
            <h1>
              <span>RyzikOS</span>
              <br />
              {release ? release.version : "1.0"}
            </h1>
            <p>
              Операционная система с нуля на ассемблере и Rust. Загружается сразу в рабочий стол с браузером, магазином
              программ, файлами и звуком.
            </p>
            <div className="actions">
              <a className="btn primary" href="/download">
                <DownloadIcon />
                Скачать ISO
              </a>
              <a className="btn ghost" href={REPO_URL}>
                Исходники на GitHub
              </a>
            </div>
            <div className="release-line">
              {release ? (
                <>
                  <span>
                    <i className="dot" />
                    Последняя сборка {release.version}
                  </span>
                  <span>{formatDate(release.date)}</span>
                  <span>{formatSize(release.size)}</span>
                </>
              ) : (
                <span>Новая сборка появляется после каждого обновления на GitHub</span>
              )}
            </div>
          </div>
          <div className="hero-shot">
            <Window title="RyzikOS" icon="/logo.png">
              <img src="/shots/hero.jpg" alt="Рабочий стол RyzikOS с App Store" width="1600" height="900" />
            </Window>
          </div>
        </section>

        <section className="section" id="features">
          <div className="reveal">
            <div className="kicker">Что внутри</div>
            <h2>Всё своё, от загрузчика до браузера</h2>
            <p className="lead">
              Ни Linux, ни Windows внутри: ядро, драйверы, окна, шрифты и программы написаны для RyzikOS. Сеть,
              HTTPS и JavaScript тоже работают.
            </p>
          </div>
          <div className="grid">
            {FEATURES.map((f) => (
              <div className="card reveal" key={f.title}>
                <img src={`/icons/${f.icon}.png`} alt="" />
                <h3>{f.title}</h3>
                <p>{f.text}</p>
              </div>
            ))}
          </div>
        </section>

        <section className="section" id="screens">
          <div className="reveal">
            <div className="kicker">Скриншоты</div>
            <h2>Так выглядит RyzikOS</h2>
            <p className="lead">Лаунчер вместо «Пуска», док внизу, строка меню сверху и окна со сглаженной графикой.</p>
          </div>
          <div className="shots">
            {SHOTS.map((s) => (
              <Window key={s.file} title={s.title} icon={`/icons/${s.icon}.png`} className={`reveal ${s.wide ? "wide" : ""}`}>
                <img src={`/shots/${s.file}.jpg`} alt={s.title} loading="lazy" />
              </Window>
            ))}
          </div>
        </section>

        <section className="section" id="install">
          <div className="reveal">
            <div className="kicker">Установка</div>
            <h2>Запуск за пару минут</h2>
            <p className="lead">В QEMU на Windows или на настоящем компьютере с флешки.</p>
          </div>
          <div className="install">
            <Window title="Запуск на Windows" icon="/icons/settings.png" className="reveal">
              <div className="steps">
                <ol>
                  <li>
                    Установите QEMU: <code>winget install SoftwareFreedomConservancy.QEMU</code>
                  </li>
                  <li>
                    Скачайте <a href="/download">последний ISO</a> и назовите его <code>everos.iso</code>.
                  </li>
                  <li>
                    Положите рядом{" "}
                    <a href={`${REPO_URL}/blob/main/scripts/run-windows.bat`}>
                      <code>run-windows.bat</code>
                    </a>{" "}
                    и запустите его двойным щелчком.
                  </li>
                  <li>
                    На экране блокировки нажмите Enter: пользователь <code>root</code> без пароля.
                  </li>
                </ol>
              </div>
            </Window>
            <Window title="Terminal" icon="/icons/terminal.png" dark className="reveal">
              <div className="terminal">
                <Typing lines={TERMINAL} />
              </div>
            </Window>
            <Window title="На настоящем компьютере" icon="/icons/computer.png" className="reveal">
              <div className="steps">
                <table className="table">
                  <tbody>
                    <tr>
                      <td>Загрузка</td>
                      <td>Флешка или диск, BIOS или UEFI с включённым CSM</td>
                    </tr>
                    <tr>
                      <td>Процессор и память</td>
                      <td>x86_64, от 512 МБ</td>
                    </tr>
                    <tr>
                      <td>Диски</td>
                      <td>IDE и SATA (AHCI), CD и DVD</td>
                    </tr>
                    <tr>
                      <td>Сеть по кабелю</td>
                      <td>Intel e1000 и e1000e (I217, I218, I219), Realtek RTL8111/8168, RTL8139</td>
                    </tr>
                    <tr>
                      <td>Звук</td>
                      <td>Ensoniq ES1370</td>
                    </tr>
                    <tr>
                      <td>Пока нет</td>
                      <td>Wi-Fi, NVMe, USB-флешки, встроенный звук HD Audio</td>
                    </tr>
                  </tbody>
                </table>
              </div>
            </Window>
            <Window title="Сборка из исходников" icon="/icons/program.png" className="reveal">
              <div className="steps">
                <ol>
                  <li>
                    Нужны Rust, <code>nasm</code>, <code>clang</code>, <code>grub-mkrescue</code>, <code>xorriso</code>,{" "}
                    <code>mtools</code> и QEMU.
                  </li>
                  <li>
                    <code>make</code> собирает ISO, <code>make run</code> запускает его в QEMU.
                  </li>
                  <li>
                    <code>make test</code> загружает систему без экрана и проверяет рабочий стол, сеть, диски и видео.
                  </li>
                </ol>
              </div>
            </Window>
          </div>
        </section>

        <section className="section" id="download">
          <Window title="App Store" icon="/icons/store.png" className="reveal">
            <div className="download">
              <img className="big-logo" src="/logo.png" alt="" />
              <div>
                <h3>{release ? release.name : "RyzikOS"}</h3>
                <div className="meta">
                  {release ? (
                    <>
                      <span className="pill ok">Последняя версия</span>
                      <span className="pill">{formatDate(release.date)}</span>
                      <span className="pill">{formatSize(release.size)}</span>
                      <span className="pill">{release.file}</span>
                    </>
                  ) : (
                    <span className="pill">Сборки публикуются на GitHub</span>
                  )}
                </div>
                <div className="actions">
                  <a className="btn primary" href="/download">
                    <DownloadIcon />
                    Скачать
                  </a>
                  <a className="btn ghost" style={{ color: "var(--accent)", borderColor: "var(--stroke)" }} href={RELEASES_URL}>
                    Все версии
                  </a>
                </div>
                <div className="hint">
                  Кнопка всегда отдаёт самую новую сборку: GitHub собирает ISO после каждого обновления кода.
                </div>
              </div>
            </div>
          </Window>
        </section>
      </main>

      <footer>
        RyzikOS — открытый проект под лицензией MIT. <a href={REPO_URL}>Исходный код на GitHub</a>.
        <br />
        Шрифты Terminus и DejaVu, JavaScript на QuickJS, сеть на smoltcp.
      </footer>

      <div className="dock-wrap">
        <nav className="dock" aria-label="Док">
          {DOCK.map((d, i) =>
            d.sep ? (
              <span className="sep" key={i} />
            ) : (
              <a key={d.tip} href={d.href} className={d.sm ? "hide-sm" : ""} {...(d.external ? { target: "_blank", rel: "noreferrer" } : {})}>
                <img src={d.icon} alt={d.tip} />
                <span className="tip">{d.tip}</span>
              </a>
            )
          )}
        </nav>
      </div>
    </>
  );
}
