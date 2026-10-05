# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo build          # build
cargo run            # run (requires an X11 display; use `DISPLAY=:0 cargo run` if not inherited)
cargo check          # fast type-check
cargo clippy         # lint
cargo test           # no tests exist yet
```

Edition 2024, so a recent toolchain is required (Rust 1.85+; developed on 1.97).

## Architecture

Catick is a single-binary Rust crate (`src/main.rs`, no modules yet) that opens an X11 window via `x11rb`. The crate is at an early/experimental stage: `main` creates one window and then loops printing every event received.

Key points in the current implementation:

- **ARGB overlay window**: `find_argb_visual` scans `screen.allowed_depths` for a depth-32 visual (32-bit = ARGB with alpha). The window is created on that visual/depth with a dedicated colormap (`create_colormap` is required for non-default visuals), 8-bit alpha `.background_pixel(0)`, `override_redirect(1)`, and no border. This combination makes a borderless, always-on-top transparent overlay window — the intended foundation for the app's drawing/animation on top of the desktop.
- `override_redirect` also means the window manager ignores it (no decorations, no placement); size/position are set directly at `create_window`.
- The event loop is a placeholder: it only `println!`s events and ignores them.
- The code compiles with a few warnings (unused `COPY_DEPTH_FROM_PARENT` import, unchecked `create_colormap` result) — treat warnings as signal to clean up when touching those lines.

Comments in existing code are written in Chinese; match the language of the surrounding comments when editing.

## Repository state

No commits exist yet on the `master` branch — everything is untracked.
