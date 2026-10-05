mod app;
mod config;
mod overlay;
mod render;
mod timer;
mod tray;
mod wayland;
mod x11;

use clap::Parser;
use config::{Backend, Config, Mode, Position};
use std::path::PathBuf;

/// Catime 风格的 Linux 桌面计时器：透明悬浮窗 + 系统托盘。
#[derive(Parser, Debug)]
#[command(name = "catick", version, about)]
struct Cli {
    /// 计时模式
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    /// 倒计时时长，如 25m / 90s / 1h30m / 25:00
    #[arg(long, value_name = "DURATION")]
    time: Option<String>,
    /// 字体族
    #[arg(long)]
    font: Option<String>,
    /// 字号（像素）
    #[arg(long, value_name = "PX")]
    font_size: Option<f64>,
    /// 文字颜色，#rrggbb
    #[arg(long)]
    color: Option<String>,
    /// 不透明度 0.0 ~ 1.0
    #[arg(long)]
    opacity: Option<f64>,
    /// 窗口后端
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// 窗口位置，如 100,100
    #[arg(long, value_name = "X,Y")]
    position: Option<String>,
    /// 鼠标穿透：窗口不拦截桌面操作（配置文件默认开启）
    #[arg(long, conflicts_with = "interactive")]
    click_through: bool,
    /// 可交互：窗口接收鼠标（关闭鼠标穿透）
    #[arg(long)]
    interactive: bool,
    /// 启动即进入编辑模式
    #[arg(long)]
    edit: bool,
    /// 不启动系统托盘
    #[arg(long)]
    no_tray: bool,
    /// 把当前渲染的一帧输出为 PNG 后退出（调试用）
    #[arg(long, value_name = "PATH")]
    dump_png: Option<PathBuf>,
    /// 打印生效后的配置（TOML）后退出
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
        println!("catick: 已输出 {w}x{h} 帧到 {}", path.display());
        return Ok(());
    }

    app::App::new(cfg)?.run()
}
