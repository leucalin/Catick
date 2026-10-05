# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo run                                  # run the app (needs a Wayland or X11 session)
cargo run -- --dump-png /tmp/frame.png     # render one frame headlessly
cargo test                                 # unit tests (config parsing)
cargo clippy --all-targets -- -D warnings  # CI enforces zero warnings
cargo fmt                                  # rustfmt defaults; CI checks --check
```

Interactive verification without xdotool (X11 backend only):

```sh
cargo run --example probe -- --hash        # pixel hash + alpha stats of the window
cargo run --example xinput -- scroll 5     # XTEST pointer/button/key injection
```

On niri, `niri msg layers` lists the layer surface and `niri msg action
screenshot-screen` (~/Pictures/Screenshots) shows what is really on screen.

## Architecture

Catick is a Catime-style desktop timer: a transparent always-on-top overlay
rendered with cairo, controlled by mouse and a StatusNotifierItem tray.

- `src/overlay.rs` — the `Overlay` trait, the only seam between display
  backends. `Input` is a normalized event type (all coordinates in "root"
  space; the Wayland backend synthesizes root coords by adding the window
  position to surface-local coords so drag math is shared with X11).
- `src/x11.rs` — depth-32 ARGB override-redirect window, banded `put_image`
  blitting, SHAPE input region for click-through.
- `src/wayland.rs` — wlr-layer-shell overlay surface, shm buffers written per
  frame, viewporter + fractional-scale so rendering happens at physical
  resolution. Pointer events are read through `prepare_read` → poll →
  `dispatch_pending` (the guard must be taken in `poll_fd` and consumed in
  `drain_events`). `PosTracker` keeps drag correct: local pointer coordinates
  are computed against the compositor's *applied* position, so virtual root
  coordinates must use the `wl_display.sync`-confirmed position, never the
  locally submitted one (that double-counts moves and flings the window).
  During a drag, absolute `wl_pointer.motion` **must be ignored** while
  `zwp_relative_pointer_v1` deltas drive the movement — mixing both makes the
  window fly away. `examples/winject.rs` (zwlr_virtual_pointer) reproduces
  drags on niri: injecting 100x50 must move the window exactly 100x50.
- `src/app.rs` — the event loop: drain backend events, tick the timer, redraw
  on change, then `poll([backend fd, tray wake pipe])` with a timeout from
  `Timer::next_update`. Everything else (interactions, edit mode, persistence)
  also lives here.
- `src/timer.rs` — mode state machine on `CLOCK_BOOTTIME` (advances while
  suspended, immune to NTP steps). Countdown-like modes are
  `end + remaining`; the display uses ceiling seconds so `00:00` only shows at
  true zero.
- `src/render.rs` — cairo rendering; `TEMPLATE` (`88:88:88`) fixes the window
  size so digits don't jitter it. `render_icon` outputs SNI network-order ARGB.
  Custom font files are registered through fontconfig
  (`register_font_file`) and then selected by family name — never hand a
  freetype `FT_Face` to `cairo::FontFace::create_from_ft`, cairo keeps its own
  FT library and cross-library faces abort the process.
- `src/icon.rs` — decodes PNG (cairo), GIF (animated, frames cached as cairo
  surfaces) and JPEG (zune-jpeg) for the image tray icon.
- `src/i18n.rs` / `src/picker.rs` — zh/en strings and the zenity/kdialog file
  dialog (always invoked from a worker thread; the result comes back as a
  `tray::Command`).
- `src/tray.rs` — ksni tray on its own thread; the icon is a cairo-drawn
  vector glyph (`render::TrayGlyph`), and the menu carries font (from
  `fc-list`) and color-preset pickers. **Callbacks may only send to the
  mpsc channel and poke the wake pipe** — never call `Handle::update` from a
  callback (deadlock); the main loop pushes state back via `update`.
- `src/config.rs` — TOML at `$XDG_CONFIG_HOME/catick/config.toml`.

Rendering contract: `present` receives premultiplied ARGB32, tightly packed,
stride = width×4 (cairo `Format::ARgb32`). The app renders at *physical* pixel
size (`phys_w/phys_h`) and lays out in *logical* coordinates (`Overlay::scale()`
is 1.0 on X11).

## Invariants worth keeping

- UI adjustments persist via `App::persist`, which merges into the on-disk
  config; never write the effective config back wholesale (that bakes one-shot
  CLI overrides into the file).
- The Wayland backend gets no modifier state; features that need Ctrl/Shift are
  X11-only and must have a tray-menu equivalent.
- niri's XWayland does not honor override-redirect placement, so on Wayland
  sessions `backend = "auto"` must select the Wayland backend; the X11 backend
  targets X11 sessions and other compositors.
- The window position is clamped to the screen at startup, while dragging and
  when resizing; a stale off-screen position in the config must not hide the
  timer.
- `[profile.release]` is deliberately size-optimized (opt-level "z", fat LTO,
  panic=abort, strip). zbus dominates the remaining ~2.3 MB; do not add heavy
  dependencies casually.
- Zero warnings and `cargo fmt` clean are CI requirements.

## Conventions

- Comments and user-facing strings are in Chinese; identifiers and commit
  messages are in English (Conventional Commits).
- Commits created by Claude end with
  `Co-Authored-By: Claude Code <noreply@anthropic.com>`.
- License: Apache-2.0. README follows Catime's structure (centered header,
  emoji sections). Catime and activate-linux are credited as references.

## Current state

Feature-complete against the plan: modes, tray, edit mode, config persistence,
blinking finish + `on_finish_cmd`, single-instance lock. Known gaps are listed
in the README's "Not (yet) implemented" section.
