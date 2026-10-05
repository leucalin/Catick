<div align="center">

# Catick

**A tiny, transparent, always-on-top timer for Linux — a Catime alternative.**

Countdown · Stopwatch · Clock · Pomodoro — rendered with cairo, living on your
desktop like a watermark.

</div>

Catick is a Linux port of [Catime](https://github.com/vladelaina/Catime), the
pure-C Windows timer. It draws the time in a borderless transparent overlay
that floats above your windows, and is fully controllable from the system tray.

## ✨ Features

- **Four modes** — clock, countdown, stopwatch, and Pomodoro with automatic
  work → short break → long break cycling.
- **Watermark-style overlay** — transparent, borderless, always on top.
  Click-through by default, so it never gets in your way.
- **Two display backends, picked automatically**
  - **Wayland** — a native `wlr-layer-shell` overlay (niri, sway, Hyprland,
    KDE…), crisp on HiDPI thanks to fractional-scale support.
  - **X11** — a depth-32 ARGB window via x11rb, for X11 desktops and other
    compositors' XWayland.
- **System tray (StatusNotifierItem)** — a vector icon (progress ring for
  timers, clock face for clock mode) shows the state at a glance, and the menu
  has everything: modes, quick presets, pause/reset, edit mode, click-through,
  12/24-hour clock, font picker, color presets, font size, opacity, quit.
- **Edit mode** — drag to move, scroll to resize the font, Ctrl+scroll for
  opacity; everything is saved as you adjust it.
- **Config file + CLI overrides** — `~/.config/catick/config.toml` persists
  your settings; `--time 25m --font-size 90` overrides them for one run.
- **Finish alert** — the text blinks and `on_finish_cmd` runs (default:
  `notify-send`), so you can hook up sounds or anything else.
- **Single instance** — a second launch refuses to start instead of stacking
  timers.

## 📥 Install

Build from source (Rust 1.92+):

```sh
git clone https://github.com/<you>/Catick.git
cd Catick
cargo build --release
./target/release/Catick          # run it
```

System dependencies:

| Distro         | Packages                              |
| -------------- | ------------------------------------- |
| Arch Linux     | `sudo pacman -S cairo wayland`        |
| Ubuntu/Debian  | `sudo apt install libcairo2-dev libwayland-dev pkg-config` |
| Fedora         | `sudo dnf install cairo-devel wayland-devel pkgconf` |

On first run a default config is written to `~/.config/catick/config.toml`.
A 25-minute countdown starts right away, just like Catime's first launch.

## 📑 User Guide

All interaction is with the mouse — no keyboard focus needed (and on Wayland,
none is possible for an overlay).

| Action                          | Effect                                   |
| ------------------------------- | ---------------------------------------- |
| Left click                      | Pause / resume                           |
| Middle click                    | Reset                                    |
| Left drag                       | Move the window                          |
| Right click                     | Enter / exit edit mode                   |
| Scroll                          | ±1 minute (Shift ±10s, Ctrl ±1h)¹        |
| Scroll in edit mode             | Font size ±2px                           |
| Ctrl + scroll in edit mode      | Opacity ±5% ¹                            |
| Tray left click                 | Enter / exit edit mode                   |
| Tray right click                | Menu (modes, presets, fonts, colors, …)  |

¹ Modifier combos need pointer modifier state, which Wayland does not deliver
to overlays. There, use the tray menu's font-size and opacity items instead.

The window is **click-through by default** (like Catime). Disable it from the
tray menu or with `catick --interactive` to use the mouse interactions above.

## ⚙️ Configuration

`~/.config/catick/config.toml` — every key is optional:

```toml
mode = "countdown"          # clock | countdown | stopwatch | pomodoro
duration = "25m"            # 90s | 25m | 1h30m | 25:00
font_family = "monospace"
font_size = 72.0
bold = true
color = "#ffffff"
opacity = 1.0
clock_24h = true
click_through = true
tray = true
backend = "auto"            # auto | wayland | x11
position = { x = 900, y = 200 }
on_finish_cmd = "notify-send Catick \"Time's up!\""
edit_on_start = false

[pomodoro]
work = "25m"
short_break = "5m"
long_break = "15m"
cycles = 4                  # work rounds before a long break
```

Useful flags: `--mode`, `--time`, `--font`, `--font-size`, `--color`,
`--opacity`, `--backend x11|wayland`, `--position 900,200`, `--interactive`,
`--no-tray`, `--edit`, `--dump-config`, `--dump-png frame.png`.

## 🛠️ Development

```sh
cargo run                      # run from source
cargo clippy --all-targets     # lint
cargo test                     # unit tests (config parsing)
cargo fmt                      # formatting
```

Verification helpers (see `examples/`):

- `cargo run --example probe -- --hash` — sample the X11 window's pixels and
  print a content hash; useful for scripted interaction tests.
- `cargo run --example xinput -- scroll 5` / `click 1` / `drag x1 y1 x2 y2` —
  synthesize pointer input over XTEST when xdotool isn't available.
- `catick --dump-png out.png` — render a frame without a display server.

On niri, `niri msg layers` lists the layer surface and
`niri msg action screenshot-screen` verifies what is actually on screen.

The release profile is tuned for size (`opt-level = "z"`, fat LTO, `panic =
"abort"`, stripped): roughly 2.3 MB, dominated by the D-Bus stack the tray
needs.

## 🚧 Not (yet) implemented

Tray icon animations/GIFs, Catime plugins, autostart entries, native global
hotkeys, and per-monitor placement on Wayland (the overlay goes on one output,
chosen by the compositor).

## 🙏 Credits

- [Catime](https://github.com/vladelaina/Catime) — the original Windows timer
  that inspired this project and defines the interaction model.
- [activate-linux](https://github.com/MrGlockenspiel/activate-linux) — the
  dual X11/layer-shell overlay approach this backend design follows.
- [cairo](https://cairographics.org/), [x11rb](https://github.com/psychon/x11rb),
  [wayland-rs](https://github.com/Smithay/wayland-rs),
  [ksni](https://github.com/iovxw/ksni).

## 📄 License

Apache-2.0 — see [LICENSE](LICENSE).
