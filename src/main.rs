mod app;
mod config;
mod i18n;
mod icon;
mod overlay;
mod picker;
mod render;
mod timer;
mod tray;
mod wayland;
mod x11;

use clap::Parser;
use config::{Backend, Config, Mode, Position};
use std::path::PathBuf;

/// A Catime-style desktop timer for Linux: transparent overlay + system tray.
#[derive(Parser, Debug)]
#[command(name = "catick", version, about)]
struct Cli {
    /// Timer mode
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    /// Countdown duration, e.g. 25m / 90s / 1h30m / 25:00
    #[arg(long, value_name = "DURATION")]
    time: Option<String>,
    /// Font family
    #[arg(long)]
    font: Option<String>,
    /// Font size in pixels
    #[arg(long, value_name = "PX")]
    font_size: Option<f64>,
    /// Text color, #rrggbb
    #[arg(long)]
    color: Option<String>,
    /// Opacity, 0.0 - 1.0
    #[arg(long)]
    opacity: Option<f64>,
    /// Window backend
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Window position, e.g. 100,100
    #[arg(long, value_name = "X,Y")]
    position: Option<String>,
    /// Click-through: the window ignores the mouse (on by default)
    #[arg(long, conflicts_with = "interactive")]
    click_through: bool,
    /// Interactive: the window receives mouse events (disables click-through)
    #[arg(long)]
    interactive: bool,
    /// Start in edit mode
    #[arg(long)]
    edit: bool,
    /// Do not start the system tray
    #[arg(long)]
    no_tray: bool,
    /// Write one rendered frame to a PNG and exit (debug)
    #[arg(long, value_name = "PATH")]
    dump_png: Option<PathBuf>,
    /// Print the effective config (TOML) and exit
    #[arg(long)]
    dump_config: bool,
}

impl Cli {
    /// 把命令行参数覆盖到配置上。
    fn apply_to(&self, cfg: &mut Config) {
        if let Some(mode) = self.mode {
            cfg.mode = mode;
        }
        if let Some(time) = &self.time {
            cfg.duration = time.clone();
        }
        if let Some(font) = &self.font {
            cfg.font_family = font.clone();
        }
        if let Some(size) = self.font_size {
            cfg.font_size = size;
        }
        if let Some(color) = &self.color {
            cfg.color = color.clone();
        }
        if let Some(opacity) = self.opacity {
            cfg.opacity = opacity;
        }
        if let Some(backend) = self.backend {
            cfg.backend = backend;
        }
        if let Some(pos) = &self.position {
            let mut it = pos.split(',');
            if let (Some(x), Some(y), None) = (it.next(), it.next(), it.next())
                && let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse())
            {
                cfg.position = Some(Position { x, y });
            }
        }
        if self.click_through {
            cfg.click_through = true;
        }
        if self.interactive {
            cfg.click_through = false;
        }
        if self.edit {
            cfg.edit_on_start = true;
        }
        if self.no_tray {
            cfg.tray = false;
        }
    }
}

/// 单实例锁：避免两个悬浮计时器叠在一起；进程退出（fd 关闭）自动释放。
fn acquire_single_instance() -> Result<std::fs::File, Box<dyn std::error::Error>> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("catick.lock");
    let file = std::fs::File::create(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err("catick is already running (if not, remove $XDG_RUNTIME_DIR/catick.lock)".into())
        }
        Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let first_run = Config::path().is_some_and(|p| !p.exists());
    let mut cfg = Config::load();
    if first_run {
        // 首次运行生成默认配置文件，方便用户直接编辑
        Config::default().save();
    }
    cli.apply_to(&mut cfg);

    if cli.dump_config {
        println!("{}", toml::to_string_pretty(&cfg)?);
        return Ok(());
    }

    if let Some(path) = &cli.dump_png {
        let now = timer::now();
        let timer = timer::Timer::new(&cfg, now);
        let style = render::Style::from_config(&cfg);
        let (w, h) = render::measure(&style, render::TEMPLATE);
        let frame = render::render(&style, &timer.display(now), w, h, false)?;
        render::dump_png(frame, path)?;
        println!("catick: wrote a {w}x{h} frame to {}", path.display());
        return Ok(());
    }

    // 调试输出（--dump-*）不需要占用实例锁
    let _lock = acquire_single_instance()?;

    let result = app::App::new(cfg)?.run();
    if let Err(err) = &result {
        eprintln!("catick: exiting: {err}");
    }
    result
}
