# MSense

Lightweight, open-source Apple Silicon hardware monitor for the macOS menu bar, written in Rust.

[English](#english) · [Русский](#русский)

---

## English

MSense keeps the most important Mac temperatures visible without taking up valuable menu-bar space. The compact status icon displays two stacked values: the maximum CPU core temperature on top and the GPU temperature below. Click it to open detailed live hardware metrics.

### Features

- Maximum CPU core and GPU temperatures directly in the menu bar
- Automatic detection of Efficiency and Performance core counts
- Temperature for every available CPU core
- Per-core CPU activity, scaled load, and frequency
- GPU activity, frequency, temperature, power, and core count
- RAM and swap usage in decimal GB, matching the units used by macOS
- CPU, GPU, Neural Engine, DRAM, GPU SRAM, compute, and system power metrics
- Cooling fan information when available
- Compact native macOS menu-bar interface
- No battery, trackpad, charger, or unrelated proximity sensor clutter
- Optional in-app Bitcoin donation dialog with copy support

### Requirements

- An Apple Silicon Mac: M1, M2, M3, M4, or newer
- macOS

Intel Macs are not supported.

### Installation

1. Open the [Releases](../../releases) page.
2. Download `MSense-0.2.0-macOS-arm64.dmg` using Safari, Chrome, or Firefox.
3. Open the DMG and drag `MSense.app` into `Applications`.
4. Launch MSense. It runs in the menu bar, so it does not show a Dock icon or a main window.

On macOS 13 or later, open MSense from `/Applications` or `~/Applications` once to enable launch at login. Use **Launch at Login** in the MSense menu to turn it off or on. If approval is required, click the same item to open System Settings.

#### First launch on macOS

Community builds are ad-hoc signed and are not notarized with a paid Apple Developer ID. If macOS blocks the first launch:

1. Try to open MSense once.
2. Open **System Settings → Privacy & Security**.
3. Scroll to **Security** and click **Open Anyway** for MSense.
4. Confirm by clicking **Open**.

This is normally required only once. If macOS shows a generic “application cannot be opened” message without **Open Anyway**, download the release from GitHub using a web browser instead of receiving the application directly as a messenger attachment.

For Telegram, use the release asset ending in `-dmg.zip`. It contains the DMG inside an additional ZIP layer.

### Build from source

Install the Rust toolchain and Apple Command Line Tools, then run:

```bash
git clone https://github.com/stem23456-coder/MSeense---Check-the-CPU-temperature-on-your-Mac.git msense
cd msense
bash build-msense-app-free.sh
```

The finished application and release packages will be created in `dist/`.

To add an application icon, place a square `AppIcon.png` of at least 1024×1024 pixels, or a ready-made `AppIcon.icns`, next to `Cargo.toml` before building.

### Privacy

MSense reads hardware metrics locally on your Mac. Monitoring data is not uploaded to a server, and the application contains no telemetry or advertising.

### Support the developer

If MSense is useful to you, you can support its development with Bitcoin:

```text
bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr
```

[Donate with Bitcoin](bitcoin:bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr)

### Contributing

Issues and pull requests are welcome. When reporting a sensor problem, include your Mac model, Apple chip, and macOS version, but do not publish personal system information.

### License

MSense is released under the MIT License.

---

## Русский

MSense — лёгкая open-source утилита для мониторинга Apple Silicon в строке меню macOS, написанная на Rust.

Верхнее число в строке меню показывает максимальную температуру среди ядер процессора, нижнее — температуру GPU. По нажатию открывается подробное меню со всеми доступными показателями.

### Возможности

- Температура CPU и GPU в строке меню
- Автоматическое определение количества эффективных и производительных ядер
- Температура, нагрузка и частота каждого ядра
- Нагрузка, частота, температура и энергопотребление GPU
- Использование оперативной памяти и swap в десятичных GB, как в macOS
- Энергопотребление CPU, GPU, Neural Engine, DRAM и всей вычислительной части
- Информация о вентиляторах, если она доступна
- Без лишних датчиков аккумулятора, трекпада, зарядного устройства и proximity
- Компактный нативный интерфейс macOS

### Требования

- Mac с Apple Silicon: M1, M2, M3, M4 или новее
- macOS

Компьютеры Mac с процессорами Intel не поддерживаются.

### Установка

1. Откройте страницу [Releases](../../releases).
2. Скачайте `MSense-0.2.0-macOS-arm64.dmg` через Safari, Chrome или Firefox.
3. Откройте DMG и перетащите `MSense.app` в папку «Программы».
4. Запустите MSense. Утилита работает в строке меню, поэтому у неё нет главного окна и значка в Dock.

На macOS 13 и новее при первом открытии из `/Applications` или `~/Applications` MSense включает запуск при входе. Галочка **Launch at Login** в меню позволяет отключить или снова включить автозапуск. Если требуется одобрение macOS, этот же пункт откроет системные настройки.

Если macOS заблокирует первый запуск, один раз попробуйте открыть MSense, затем перейдите в **Системные настройки → Конфиденциальность и безопасность → Безопасность** и нажмите **Открыть всё равно**.

Для отправки через Telegram используйте файл, название которого заканчивается на `-dmg.zip`. Внутри него находится DMG, дополнительно упакованный в ZIP.

### Сборка из исходного кода

```bash
git clone https://github.com/stem23456-coder/MSeense---Check-the-CPU-temperature-on-your-Mac.git msense
cd msense
bash build-msense-app-free.sh
```

Готовые файлы появятся в папке `dist/`.

### Поддержать разработчика

Если MSense оказался полезен, вы можете поддержать разработку через Bitcoin:

```text
bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr
```

[Отправить Bitcoin](bitcoin:bc1qsud09kj6pdng2cn7tzknajtzvmyar9hr29mkzr)

### Лицензия

Проект распространяется по лицензии MIT.
