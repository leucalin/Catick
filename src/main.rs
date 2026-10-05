mod config;
mod overlay;
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

    let mut ov = overlay::create(&cfg)?;

    // TODO(步骤 4): 窗口尺寸与位置由 cairo 的文字测量结果决定，
    // 这里先用固定尺寸验证「创建窗口 → 贴帧 → 事件循环」整条链路。
    let (screen_w, screen_h) = ov.screen_size();
    let (w, h) = (320u32, 120u32);
    let (x, y) = cfg.position.map(|p| (p.x, p.y)).unwrap_or((
        ((screen_w.saturating_sub(w)) / 2) as i32,
        (screen_h / 5) as i32,
    ));
    ov.set_geometry(x, y, w, h)?;
    ov.set_passthrough(cfg.click_through)?;
    let frame = test_pattern(w, h);
    ov.present(&frame)?;
    println!(
        "catick: 已显示测试窗口 {w}x{h} @ ({x},{y})，click_through={}，模式={:?} 时长={:?} 颜色={:?}（Ctrl+C 退出）",
        cfg.click_through,
        cfg.mode,
        cfg.duration(),
        cfg.color_rgb(),
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
                    ov.present(&frame)?;
                }
            }
        }
        ov.flush()?;
    }
}

/// 步骤 3 的占位帧：半透明填充 + 亮边，用于肉眼确认贴图与透明效果。
/// 数值为预乘 alpha 的 ARGB32（小端字节序 B,G,R,A）。
fn test_pattern(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h {
        for x in 0..w {
            let i = ((y as usize * w as usize) + x as usize) * 4;
            let border = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
            if border {
                buf[i..i + 4].copy_from_slice(&[230, 230, 230, 230]);
            } else {
                buf[i..i + 4].copy_from_slice(&[60, 30, 20, 160]);
            }
        }
    }
    buf
}
