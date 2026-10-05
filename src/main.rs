mod config;
mod overlay;
mod render;
mod wayland;
mod x11;

use clap::Parser;
use config::{Backend, Config, Mode, Position};
use overlay::Input;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::time::Timespec;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::time::Duration;

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

    // 渲染参数：窗口尺寸取「模板字符串」的度量，避免秒数变化导致宽度抖动
    let style = render::Style::from_config(&cfg);
    let template = TIMER_TEMPLATE;
    let text = placeholder_text(&cfg);

    if let Some(path) = &cli.dump_png {
        let (w, h) = render::measure(&style, template);
        let frame = render::render(&style, &text, w, h)?;
        render::dump_png(frame, path)?;
        println!("catick: 已输出 {w}x{h} 帧到 {}", path.display());
        return Ok(());
    }

    let mut ov = overlay::create(&cfg)?;
    let (screen_w, screen_h) = ov.screen_size();
    let (w, h) = render::measure(&style, template);
    let (x, y) = cfg.position.map(|p| (p.x, p.y)).unwrap_or((
        ((screen_w.saturating_sub(w)) / 2) as i32,
        (screen_h / 5) as i32,
    ));
    ov.set_geometry(x, y, w, h)?;
    ov.set_passthrough(cfg.click_through)?;
    let frame = render::render(&style, &text, w, h)?;
    ov.present(&frame.pixels)?;
    println!(
        "catick: 已显示 {w}x{h} @ ({x},{y})，click_through={}，模式={:?} 文本={text:?}（Ctrl+C 退出）",
        cfg.click_through, cfg.mode,
    );

    // TODO(步骤 5): 换成 App 事件循环（计时调度、交互、托盘命令）。
    let fd = {
        // SAFETY: fd 由 ov 持有，在 ov 存活期间始终有效
        unsafe { BorrowedFd::borrow_raw(ov.event_fd()) }
    };
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    loop {
        let timeout = Timespec::try_from(Duration::from_millis(500))?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
        }
        for ev in ov.drain_events() {
            match ev {
                Input::ButtonPress { button, root_x, root_y } => {
                    println!("press button={button} @ {root_x},{root_y}");
                }
                Input::ButtonRelease { button, root_x, root_y } => {
                    println!("release button={button} @ {root_x},{root_y}");
                }
                Input::Motion { root_x, root_y } => {
                    println!("motion @ {root_x},{root_y}");
                }
                Input::Redraw => {
                    let (gx, gy, gw, gh) = ov.geometry();
                    println!("redraw @ {gx},{gy} {gw}x{gh}");
                    ov.present(&frame.pixels)?;
                }
            }
        }
        ov.flush()?;
    }
}

/// 尺寸测量用的模板：所有计时模式都是等宽的 `HH:MM:SS`。
const TIMER_TEMPLATE: &str = "88:88:88";

/// TODO(步骤 5): 由 timer 状态机给出显示文本；此前先展示配置的倒计时时长。
fn placeholder_text(cfg: &Config) -> String {
    let secs = cfg.duration().as_secs();
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}
