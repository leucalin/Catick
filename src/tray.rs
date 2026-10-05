//! 系统托盘（StatusNotifierItem / D-Bus）。
//!
//! 托盘服务跑在自己的线程上；菜单/左键回调只做两件事：往 mpsc 里塞命令、
//! 往唤醒管道写一个字节。主循环据此醒来处理命令并回推新状态
//! （`Handle::update`），回调里绝不能调用 `update`（会死锁）。

use crate::config::{COLOR_PRESETS, Config, Mode};
use crate::render::{self, Style};
use crate::timer::{PomodoroPhase, Timer, now};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, ToolTip, Tray};
use rustix::fd::OwnedFd;
use std::sync::mpsc::{Receiver, Sender};

/// 托盘发回主循环的命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    TogglePause,
    Reset,
    SetMode(Mode),
    /// 快捷预设（秒）；切到倒计时并以该时长重新开始
    SetPreset(u64),
    ToggleEditMode,
    ToggleClickThrough,
    ToggleClockFormat,
    FontSizeDelta(f64),
    OpacityDelta(f64),
    SetFont(String),
    SetColor(String),
    Quit,
}

/// 主循环推给托盘的显示状态。
#[derive(Clone, Default)]
pub struct TrayState {
    pub display: String,
    pub mode: Mode,
    pub running: bool,
    pub edit_mode: bool,
    pub click_through: bool,
    pub clock_24h: bool,
    pub phase: Option<PomodoroPhase>,
    /// 可选字体（系统已安装的常用字体）与当前字体
    pub fonts: Vec<String>,
    pub current_font: String,
    /// 当前颜色 `#rrggbb`（与 COLOR_PRESETS 比较选中项）
    pub current_color: String,
    /// 22px / 44px 图标（网络字节序 ARGB32，主线程渲染）
    pub icon_small: Vec<u8>,
    pub icon_big: Vec<u8>,
}

pub struct CatickTray {
    pub state: TrayState,
    tx: Sender<Command>,
    wake: OwnedFd,
}

/// 主循环持有的托盘句柄。
pub struct TrayChannels {
    pub handle: Handle<CatickTray>,
    pub rx: Receiver<Command>,
    /// 唤醒管道的读端（已设为非阻塞）
    pub wake_read: OwnedFd,
}

impl CatickTray {
    fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
        // 唤醒主循环；管道满了也无所谓，反正已经有信号了
        let _ = rustix::io::write(&self.wake, b"c");
    }
}

impl Tray for CatickTray {
    fn id(&self) -> String {
        "catick".into()
    }

    fn title(&self) -> String {
        "Catick".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let mut icons = Vec::new();
        if !self.state.icon_small.is_empty() {
            icons.push(Icon {
                width: 22,
                height: 22,
                data: self.state.icon_small.clone(),
            });
        }
        if !self.state.icon_big.is_empty() {
            icons.push(Icon {
                width: 44,
                height: 44,
                data: self.state.icon_big.clone(),
            });
        }
        icons
    }

    fn tool_tip(&self) -> ToolTip {
        let mut lines = vec![format!("Catick · {}", self.state.display)];
        if let Some(phase) = self.state.phase {
            lines.push(phase.label().to_string());
        }
        if self.state.mode != Mode::Clock && !self.state.running {
            lines.push("已暂停".into());
        }
        ToolTip {
            title: "Catick".into(),
            description: lines.join("<br>"),
            ..Default::default()
        }
    }

    /// 左键：进入 / 退出编辑模式（对齐 Catime 的托盘交互）。
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::ToggleEditMode);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let pause_label = if self.state.running {
            "暂停"
        } else {
            "继续"
        };
        let mode_index = match self.state.mode {
            Mode::Clock => 0,
            Mode::Countdown => 1,
            Mode::Stopwatch => 2,
            Mode::Pomodoro => 3,
        };

        vec![
            StandardItem {
                label: pause_label.into(),
                activate: Box::new(|t: &mut Self| t.send(Command::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "重置".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Reset)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: "模式".into(),
                submenu: vec![
                    RadioGroup {
                        selected: mode_index,
                        select: Box::new(|t: &mut Self, index: usize| {
                            let mode = match index {
                                0 => Mode::Clock,
                                1 => Mode::Countdown,
                                2 => Mode::Stopwatch,
                                _ => Mode::Pomodoro,
                            };
                            t.send(Command::SetMode(mode));
                        }),
                        options: ["时钟", "倒计时", "秒表", "番茄钟"]
                            .iter()
                            .map(|label| RadioItem {
                                label: (*label).into(),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "快捷预设".into(),
                submenu: [1u64, 5, 15, 25, 45]
                    .iter()
                    .map(|minutes| {
                        MenuItem::from(StandardItem {
                            label: format!("{minutes} 分钟"),
                            activate: Box::new(move |t: &mut Self| {
                                t.send(Command::SetPreset(minutes * 60))
                            }),
                            ..Default::default()
                        })
                    })
                    .collect(),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "编辑模式".into(),
                checked: self.state.edit_mode,
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleEditMode)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "鼠标穿透".into(),
                checked: self.state.click_through,
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleClickThrough)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "24 小时制".into(),
                checked: self.state.clock_24h,
                activate: Box::new(|t: &mut Self| t.send(Command::ToggleClockFormat)),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "字体".into(),
                submenu: vec![
                    RadioGroup {
                        selected: self
                            .state
                            .fonts
                            .iter()
                            .position(|f| f == &self.state.current_font)
                            .unwrap_or(usize::MAX),
                        select: Box::new(|t: &mut Self, index: usize| {
                            if let Some(font) = t.state.fonts.get(index) {
                                t.send(Command::SetFont(font.clone()));
                            }
                        }),
                        options: self
                            .state
                            .fonts
                            .iter()
                            .map(|font| RadioItem {
                                label: font.clone(),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "颜色".into(),
                submenu: vec![
                    RadioGroup {
                        selected: COLOR_PRESETS
                            .iter()
                            .position(|(_, hex)| *hex == self.state.current_color)
                            .unwrap_or(usize::MAX),
                        select: Box::new(|t: &mut Self, index: usize| {
                            if let Some((_, hex)) = COLOR_PRESETS.get(index) {
                                t.send(Command::SetColor((*hex).to_string()));
                            }
                        }),
                        options: COLOR_PRESETS
                            .iter()
                            .map(|(name, _)| RadioItem {
                                label: (*name).into(),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "字号".into(),
                submenu: vec![
                    StandardItem {
                        label: "增大".into(),
                        activate: Box::new(|t: &mut Self| t.send(Command::FontSizeDelta(2.0))),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "减小".into(),
                        activate: Box::new(|t: &mut Self| t.send(Command::FontSizeDelta(-2.0))),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "透明度".into(),
                submenu: vec![
                    StandardItem {
                        label: "提高".into(),
                        activate: Box::new(|t: &mut Self| t.send(Command::OpacityDelta(0.05))),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "降低".into(),
                        activate: Box::new(|t: &mut Self| t.send(Command::OpacityDelta(-0.05))),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "退出".into(),
                activate: Box::new(|t: &mut Self| t.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("catick: StatusNotifier 宿主不可用（{reason:?}），托盘图标暂时隐藏");
        true
    }
}

/// 启动托盘服务；失败时由调用方决定是否降级运行。
pub fn spawn(state: TrayState) -> Result<TrayChannels, Box<dyn std::error::Error>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (wake_read, wake_write) = rustix::pipe::pipe()?;
    rustix::fs::fcntl_setfl(&wake_read, rustix::fs::OFlags::NONBLOCK)?;
    let tray = CatickTray {
        state,
        tx,
        wake: wake_write,
    };
    // 没有 watcher 时不报错退出，而是等待宿主出现（niri + 无托盘栏的场景）
    let handle = tray.assume_sni_available(true).spawn()?;
    Ok(TrayChannels {
        handle,
        rx,
        wake_read,
    })
}

/// 依据当前计时状态构造托盘状态（含图标渲染）。
pub fn build_state(cfg: &Config, timer: &Timer, style: &Style, fonts: &[String]) -> TrayState {
    let moment = now();
    let display = timer.display(moment);
    let glyph = render::TrayGlyph {
        fraction: timer.progress(moment),
    };
    // 暂停时图标整体变淡，状态一眼可见
    let dimmed = !timer.is_running() && timer.mode() != Mode::Clock;
    let icon_small = render::render_icon(glyph, 22, style.color, dimmed).unwrap_or_default();
    let icon_big = render::render_icon(glyph, 44, style.color, dimmed).unwrap_or_default();
    TrayState {
        display,
        mode: timer.mode(),
        running: timer.is_running(),
        edit_mode: false,
        click_through: cfg.click_through,
        clock_24h: cfg.clock_24h,
        phase: (timer.mode() == Mode::Pomodoro).then(|| timer.phase()),
        fonts: fonts.to_vec(),
        current_font: style.family.clone(),
        current_color: normalize_hex(&cfg.color),
        icon_small,
        icon_big,
    }
}

/// 规范化颜色写法，便于与预设比较（小写、带 #）。
fn normalize_hex(color: &str) -> String {
    format!(
        "#{}",
        color.trim().trim_start_matches('#').to_ascii_lowercase()
    )
}
