# RyzikOS 1.0

RyzikOS — любительская операционная система для x86_64, написанная с нуля на
ассемблере и Rust. Она загружается сразу в графический рабочий стол
1920x1080 со своими программами, браузером, магазином приложений и файлами
на диске.

![Рабочий стол](docs/screenshots/1.0/desktop-icons.png)

## Что нового в 1.0

- **Все диски в «Файлах».** Драйверы IDE и SATA (AHCI) находят все жёсткие
  диски и CD/DVD-приводы. В «Файлах» есть раздел «Компьютер»: плитка на
  каждый диск со свободным местом и состоянием.
- **App Store.** Магазин программ с вкладками «Игры», «Инструменты» и
  «Установленные». Программы скачиваются из этого репозитория через
  интернет, а без интернета ставятся с диска RyzikOS.
- **Программы в своих окнах.** Установленная программа открывается в
  отдельном окне со своим названием и значком, а не во вкладке браузера.
- **Ярлыки на рабочем столе.** После установки программы на рабочем столе
  появляется её ярлык. Все значки можно перетаскивать, и они остаются на
  своих местах.
- **Фото и Видео.** Галерея с просмотром картинок и видеоплеер (MJPEG AVI).
- **Браузер** открывает сайты на jQuery, в том числе Википедию: раньше такие
  страницы были пустыми.
- **Звук.** Драйвер звуковой карты Ensoniq ES1370. Когда отпускаешь
  ползунок громкости, звучит короткий сигнал, как в Windows и macOS.
- **Меньше Windows.** Лаунчер вместо меню «Пуск», док внизу, строка меню
  сверху, «Files», «Computer», «Trash», пути вида `/Users/root`, сочетания
  клавиш с Super.

## Что умеет

### Рабочий стол

- Экран блокировки с часами и панель входа. Сначала есть пользователь
  `root` без пароля: достаточно нажать Enter.
- Строка меню сверху: название активной программы, раскладка ENG/РУС, сеть,
  громкость, дата и время. Док внизу: закреплённые и открытые программы.
- Лаунчер (кнопка с логотипом или клавиша Super): сетка программ, поиск и
  ряд установленных программ.
- Окна со сглаженной графикой, тенями и анимациями. Их можно двигать,
  сворачивать и закрывать. Есть несколько рабочих столов и обзор окон.
- Значки на рабочем столе перетаскиваются куда угодно и прилипают к
  сетке. Правый клик по рабочему столу → «Arrange icons» выстраивает их
  заново. Файлы можно перетащить в папку или в корзину.
- Тёмная и светлая тема, обои (в том числе свои картинки).

### Программы

| Программа | Что делает |
| --- | --- |
| Files | Проводник: «Компьютер» со всеми дисками, папки пользователя, поиск, переименование, корзина |
| App Store | Магазин программ: установка через интернет или с диска, удаление, ярлык на рабочем столе |
| Browser | Веб-браузер со вкладками, HTTPS, CSS и JavaScript (подробнее ниже) |
| Photos | Галерея картинок из домашних папок, дисков и CD, просмотр с масштабом и поворотом |
| Video Player | Видео MJPEG AVI с перемоткой и повтором |
| Text Editor | Блокнот: открыть, сохранить, отмена, буфер обмена, UTF-8 и Windows-1251 |
| Draw | Рисование мышью: цвета, кисть, ластик, заливка |
| Terminal | Командная строка (список команд ниже) |
| Calculator | Калькулятор мышью или с клавиатуры |
| Settings | Экран, сеть, язык, пользователи, оформление, сведения о системе |

### Программы из App Store

Программы — это файлы `.rzapp` (HTML и JavaScript в одном файле) из папки
[`programs/`](programs). Список для магазина лежит в
[`programs/catalog.txt`](programs/catalog.txt): одна строка на программу в
виде `файл | название | категория | описание`. Чтобы добавить программу,
положите её в `programs/` и допишите строку в `catalog.txt`.

Сейчас есть 2048, Змейка, Блоки, Сапёр, Мемори, Крестики-нолики, Список
дел, Конвертер единиц и Секундомер.

![App Store](docs/screenshots/1.0/app-store.png)
![Программа в своём окне](docs/screenshots/1.0/program-window.png)

### Диски и файлы

- Жёсткие диски IDE и SATA, файловая система FAT32 с длинными именами.
  Пустой диск RyzikOS сама размечает и форматирует, поэтому Windows и Linux
  потом могут открыть его.
- Первый диск — системный. Остальные диски FAT32 видны как `/Disk 2`,
  `/Disk 3` и так далее. Диски в других форматах (NTFS, ext4) показываются
  в списке, но открыть их нельзя.
- CD и DVD читаются (ISO 9660 с длинными именами) и видны как `/Disc`,
  `/Disc 2`. На диске RyzikOS есть примеры фото, видео и программы.
- У каждого пользователя своя папка `/Users/<имя>` с Desktop, Documents,
  Downloads, Pictures, Programs. Без жёсткого диска файлы живут в памяти до
  перезагрузки.

![Компьютер](docs/screenshots/1.0/files-computer.png)

### Сеть и браузер

- Сетевая карта Intel e1000, TCP/IP ([smoltcp](https://github.com/smoltcp-rs/smoltcp)),
  DHCP и DNS. В QEMU нужен флаг `-nic user,model=e1000`.

### Звук

- Звуковая карта Ensoniq AudioPCI ES1370 (её эмулирует QEMU):
  16 бит, стерео, 22050 Гц, воспроизведение через DMA.
- Громкость задаётся ползунком в быстрых настройках (значок динамика в
  строке меню). Когда ползунок отпускаешь, звучит короткий сигнал новой
  громкости. Команда `beep` в терминале играет его же.
- В QEMU нужен флаг `-device ES1370` (на Windows
  `-audiodev dsound,id=snd0 -device ES1370,audiodev=snd0`, как в
  `run-windows.bat`). Без карты RyzikOS работает молча.
- HTTP/1.1 и HTTPS (TLS 1.3, [embedded-tls](https://github.com/drogue-iot/embedded-tls)),
  перенаправления, cookie, загрузка страниц в фоне.
- Свой движок страниц: HTML, CSS (flexbox, grid, таблицы, градиенты),
  JavaScript на [QuickJS](https://bellard.org/quickjs/) с DOM, таймерами,
  `fetch`, `localStorage`; картинки PNG и JPEG.
- Скачанные файлы сохраняются в Downloads, программы `.rzapp`
  устанавливаются в Programs.

Честно о пределах: страницы и видео пока без звука, нет `<canvas>`, WebGL, WebSocket, SVG и шрифтов с
сайтов, поэтому YouTube, VK и похожие сайты не заработают. Очень тяжёлые
страницы в QEMU без аппаратного ускорения грузятся медленно. Сертификаты
HTTPS не проверяются.

### Команды терминала

`help`, `clear`, `echo`, `info`, `ls` (`dir`), `cd`, `pwd`, `cat`, `mkdir`,
`rm`, `echo текст > файл`, `beep` (сигнал на звуковой карте), `open <файл>` (открыть файл или запустить
программу), `drives` (список дисков), `disc`, `store`, `browser [адрес]`,
`fetch <адрес>`, `notepad`, `explorer`, `photos`, `video`, `paint`, `calc`,
`settings`, `about`, `theme dark|light`, `wallpaper`, `whoami`, `users`,
`useradd`, `passwd`, `lock`, `restart`, `shutdown`, `exit`.

## Запуск на Windows

1. Установите QEMU: `winget install SoftwareFreedomConservancy.QEMU` или с
   [qemu.weilnetz.de/w64](https://qemu.weilnetz.de/w64/).
2. Возьмите ISO RyzikOS. Сборка из исходников даёт `build/everos.iso`
   (имя файла пока осталось от EverOS). Переименуйте его в `everos.iso`.
3. Положите рядом [`scripts/run-windows.bat`](scripts/run-windows.bat) и
   запустите его двойным щелчком. Он создаст диск `everos-disk.vhd` на
   128 МБ для файлов и запустит QEMU.

Или вручную в PowerShell из папки с ISO:

```powershell
& "C:\Program Files\qemu\qemu-img.exe" create -f vpc -o subformat=fixed everos-disk.vhd 128M
& "C:\Program Files\qemu\qemu-system-x86_64.exe" -cdrom everos.iso -boot d -m 512M -nic user,model=e1000 -audiodev dsound,id=snd0 -device ES1370,audiodev=snd0 -drive file=everos-disk.vhd,format=vpc,if=ide,index=0,media=disk
```

Файлы из RyzikOS можно посмотреть в Windows: закройте QEMU и дважды
щёлкните `everos-disk.vhd`. Перед следующим запуском извлеките этот диск.
Если окно не помещается на экран, нажмите Ctrl+Alt+F.

Чтобы проверить SATA и несколько дисков, запустите QEMU с `-machine q35` и
добавьте диски через `-device ide-hd,bus=ide.N`.

## Сборка из исходников

Нужны Rust (stable, через [rustup](https://rustup.rs); цель поставится сама
из `rust-toolchain.toml`), `nasm`, `clang`, `make`, `grub-mkrescue`,
`xorriso`, `mtools` и QEMU. На Ubuntu и Debian:

```sh
sudo apt install nasm build-essential clang grub-pc-bin grub-common xorriso mtools qemu-system-x86
```

```sh
make        # собрать build/everos.iso
make run    # запустить в окне QEMU с диском build/disk.img
make test   # загрузить без экрана и проверить рабочий стол, ввод, сеть, диск, CD и видео
make clean  # удалить сборку (диск build/disk.img остаётся)
```

Чтобы проверить App Store со своим каталогом, соберите с
`RYZIKOS_CATALOG=http://10.0.2.2:8000/ make` и раздайте папку с
`catalog.txt` и программами на порту 8000.

## Как устроено

| Путь | Что там |
| --- | --- |
| `boot/` | загрузка: заголовок multiboot2, переход в 64-битный режим, прерывания |
| `kernel/src/gui/` | рабочий стол и программы: `mod.rs` (окна), `taskbar.rs` (док и строка меню), `start.rs` (лаунчер), `deskicons.rs` (значки), `explorer.rs`, `store.rs`, `browser.rs`, `photos.rs`, `video.rs`, `notepad.rs` и другие |
| `kernel/src/fs/` | диски и файлы: `ata.rs` (IDE), `ahci.rs` (SATA), `drive.rs`, `fat.rs` (FAT32), `iso9660.rs` (CD), `mod.rs` (пути и монтирование) |
| `kernel/src/net/`, `kernel/src/pci.rs` | сетевая карта e1000, TCP/IP, DHCP, DNS, шина PCI |
| `kernel/src/web/` | движок браузера: HTTP и HTTPS, DOM, CSS, раскладка, картинки, мост к JavaScript, каталог программ |
| `kernel/src/js/`, `kernel/quickjs/` | движок JavaScript QuickJS |
| `kernel/src/sound.rs` | звуковая карта ES1370 и сигнал громкости |
| `kernel/src/shell.rs` | терминал |
| `programs/` | программы `.rzapp` и `catalog.txt` для App Store |
| `iso/media/`, `scripts/gen-media.py` | фото и видео для CD |
| `scripts/boot-test.sh` | автоматическая проверка загрузки в QEMU |

## Лицензия

MIT, см. [LICENSE](LICENSE). Шрифт Terminus Font (c) Dimitar Toshkov Zhekov,
SIL Open Font License 1.1, см. [fonts/LICENSE.terminus](fonts/LICENSE.terminus).
Шрифты DejaVu: Bitstream Vera License и public domain, см.
[fonts/LICENSE.dejavu](fonts/LICENSE.dejavu).
