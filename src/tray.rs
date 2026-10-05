//! 系统托盘（StatusNotifierItem / D-Bus）。
//!
//! 托盘服务跑在自己的线程上；菜单/左键回调只做两件事：往 mpsc 里塞命令、
//! 往唤醒管道写一个字节。主循环据此醒来处理命令并回推新状态
//! （`Handle::update`），回调里绝不能调用 `update`（会死锁）。

use crate::config::{COLOR_PRESETS, Config, Mode, TrayIconKind};
use crate::i18n::Lang;
use crate::render::Style;
use crate::timer::{PomodoroPhase, Timer, now};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, ToolTip, Tray};
use rustix::fd::OwnedFd;
use std::sync::mpsc::{Receiver, Sender};

/// StatusNotifierItem 的 `Id`。
///
/// 前缀是**零宽空格**：宿主（实测 DankMaterialShell）在图标位图异步加载期间会
/// 回落到「Id 的首字母」作为占位符，于是每次换图标都会闪出一个 "C"。
/// 首位放不可见字符后，占位符渲染为空白，Id 本身仍是有效标识。
const TRAY_ID: &str = "\u{200B}catick";

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
    /// 选择内置字体族
    SetFont(String),
    /// 弹出文件选择框选字体文件
    BrowseFont,
    SetFontFile(String),
    SetColor(String),
    SetTrayIcon(TrayIconKind),
    /// 弹出文件选择框选托盘图片
    BrowseTrayIcon,
    SetTrayIconFile(String),
    SetLanguage(Lang),
    /// 用系统文件管理器打开配置目录
    OpenConfigDir,
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
    pub lang: Lang,
    /// 图片图标是否已生效（菜单里勾选「图片」）
    pub icon_is_image: bool,
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
    /// 主循环从其它线程（文件选择框）投递命令用
    pub tx: Sender<Command>,
    pub wake_write: OwnedFd,
}

impl CatickTray {
    fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
        // 唤醒主循环；管道满了也无所谓，反正已经有信号了
        let _ = rustix::io::write(&self.wake, b"c");
    }
}

/// 构造标准菜单项的小工具。
fn item<F: Fn(&mut CatickTray) + Send + 'static>(label: &str, f: F) -> MenuItem<CatickTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(f),
        ..Default::default()
    }
    .into()
}

impl Tray for CatickTray {
    fn id(&self) -> String {
        TRAY_ID.into()
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
        let t = self.state.lang.t();
        let mut lines = vec![format!("Catick · {}", self.state.display)];
        if let Some(phase) = self.state.phase {
            lines.push(
                match phase {
                    PomodoroPhase::Work => t.phase_work,
                    PomodoroPhase::ShortBreak => t.phase_short,
                    PomodoroPhase::LongBreak => t.phase_long,
                }
                .to_string(),
            );
        }
        if self.state.mode != Mode::Clock && !self.state.running {
            lines.push(t.paused.to_string());
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
        let t = self.state.lang.t();
        let en = self.state.lang.resolved() == Lang::En;
        let pause_label = if self.state.running {
            t.pause
        } else {
            t.resume
        };
        // 时钟模式下没有暂停/重置的概念
        let timer_actions = self.state.mode != Mode::Clock;
        let mode_index = match self.state.mode {
            Mode::Clock => 0,
            Mode::Countdown => 1,
            Mode::Stopwatch => 2,
            Mode::Pomodoro => 3,
        };
        let icon_index = usize::from(self.state.icon_is_image);
        let lang_index = usize::from(en);

        vec![
            StandardItem {
                label: pause_label.into(),
                enabled: timer_actions,
                activate: Box::new(|tray: &mut Self| tray.send(Command::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t.reset.into(),
                enabled: timer_actions,
                activate: Box::new(|tray: &mut Self| tray.send(Command::Reset)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: t.mode.into(),
                submenu: vec![
                    RadioGroup {
                        selected: mode_index,
                        select: Box::new(|tray: &mut Self, index: usize| {
                            let mode = match index {
                                0 => Mode::Clock,
                                1 => Mode::Countdown,
                                2 => Mode::Stopwatch,
                                _ => Mode::Pomodoro,
                            };
                            tray.send(Command::SetMode(mode));
                        }),
                        options: [t.clock, t.countdown, t.stopwatch, t.pomodoro]
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
                label: t.presets.into(),
                submenu: [1u64, 5, 15, 25, 45]
                    .iter()
                    .map(|minutes| {
                        item(&format!("{minutes} {}", t.minute), move |tray| {
                            tray.send(Command::SetPreset(minutes * 60));
                        })
                    })
                    .collect(),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: t.edit_mode.into(),
                checked: self.state.edit_mode,
                activate: Box::new(|tray: &mut Self| tray.send(Command::ToggleEditMode)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: t.click_through.into(),
                checked: self.state.click_through,
                activate: Box::new(|tray: &mut Self| tray.send(Command::ToggleClickThrough)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            // 外观与行为相关的选项统一收进「设置」
            SubMenu {
                label: t.settings.into(),
                submenu: vec![
                    CheckmarkItem {
                        label: t.clock_24h.into(),
                        checked: self.state.clock_24h,
                        activate: Box::new(|tray: &mut Self| tray.send(Command::ToggleClockFormat)),
                        ..Default::default()
                    }
                    .into(),
                    MenuItem::Separator,
                    SubMenu {
                        label: t.font.into(),
                        submenu: vec![
                            RadioGroup {
                                selected: self
                                    .state
                                    .fonts
                                    .iter()
                                    .position(|f| f == &self.state.current_font)
                                    .unwrap_or(usize::MAX),
                                select: Box::new(|tray: &mut Self, index: usize| {
                                    if let Some(font) = tray.state.fonts.get(index) {
                                        tray.send(Command::SetFont(font.clone()));
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
                            MenuItem::Separator,
                            item(t.font_more, |tray| tray.send(Command::BrowseFont)),
                        ],
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: t.color.into(),
                        submenu: vec![
                            RadioGroup {
                                selected: COLOR_PRESETS
                                    .iter()
                                    .position(|(_, _, hex)| *hex == self.state.current_color)
                                    .unwrap_or(usize::MAX),
                                select: Box::new(|tray: &mut Self, index: usize| {
                                    if let Some((_, _, hex)) = COLOR_PRESETS.get(index) {
                                        tray.send(Command::SetColor((*hex).to_string()));
                                    }
                                }),
                                options: COLOR_PRESETS
                                    .iter()
                                    .map(|(zh, en_name, _)| RadioItem {
                                        label: if en { (*en_name).into() } else { (*zh).into() },
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
                        label: t.font_size.into(),
                        submenu: vec![
                            item(t.increase, |tray| tray.send(Command::FontSizeDelta(2.0))),
                            item(t.decrease, |tray| tray.send(Command::FontSizeDelta(-2.0))),
                        ],
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: t.opacity.into(),
                        submenu: vec![
                            item(t.raise, |tray| tray.send(Command::OpacityDelta(0.05))),
                            item(t.lower, |tray| tray.send(Command::OpacityDelta(-0.05))),
                        ],
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: t.tray_icon.into(),
                        submenu: vec![
                            RadioGroup {
                                selected: icon_index,
                                select: Box::new(|tray: &mut Self, index: usize| {
                                    tray.send(Command::SetTrayIcon(if index == 1 {
                                        TrayIconKind::Image
                                    } else {
                                        TrayIconKind::Ring
                                    }));
                                }),
                                options: [t.icon_ring, t.icon_image]
                                    .iter()
                                    .map(|label| RadioItem {
                                        label: (*label).into(),
                                        ..Default::default()
                                    })
                                    .collect(),
                            }
                            .into(),
                            MenuItem::Separator,
                            item(t.icon_image, |tray| tray.send(Command::BrowseTrayIcon)),
                        ],
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: t.language.into(),
                        submenu: vec![
                            RadioGroup {
                                selected: lang_index,
                                select: Box::new(|tray: &mut Self, index: usize| {
                                    tray.send(Command::SetLanguage(if index == 1 {
                                        Lang::En
                                    } else {
                                        Lang::Zh
                                    }));
                                }),
                                options: [Lang::Zh, Lang::En]
                                    .iter()
                                    .map(|lang| RadioItem {
                                        label: lang.label().into(),
                                        ..Default::default()
                                    })
                                    .collect(),
                            }
                            .into(),
                        ],
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            item(t.open_config_dir, |tray| tray.send(Command::OpenConfigDir)),
            MenuItem::Separator,
            item(t.quit, |tray| tray.send(Command::Quit)),
        ]
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("catick: StatusNotifier host unavailable ({reason:?}); tray icon hidden for now");
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
        tx: tx.clone(),
        wake: wake_write.try_clone()?,
    };
    // 没有 watcher 时不报错退出，而是等待宿主出现（niri + 无托盘栏的场景）
    let handle = tray.assume_sni_available(true).spawn()?;
    Ok(TrayChannels {
        handle,
        rx,
        wake_read,
        tx,
        wake_write,
    })
}

/// 依据当前计时状态构造托盘状态（含图标渲染）。
pub fn build_state(
    cfg: &Config,
    timer: &Timer,
    style: &Style,
    fonts: &[String],
    icons: (Vec<u8>, Vec<u8>, bool),
) -> TrayState {
    let moment = now();
    let display = timer.display(moment);
    let (icon_small, icon_big, icon_is_image) = icons;

    TrayState {
        display,
        mode: timer.mode(),
        running: timer.is_running(),
        edit_mode: false,
        click_through: cfg.click_through,
        clock_24h: cfg.clock_24h,
        phase: (timer.mode() == Mode::Pomodoro).then(|| timer.phase()),
        lang: cfg.language,
        icon_is_image,
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

#[cfg(test)]
mod tests {
    /// Id 的首字符必须不可见：宿主在图标加载期间会把它当作占位符显示。
    #[test]
    fn tray_id_starts_with_invisible_char() {
        let first = super::TRAY_ID.chars().next().expect("id 非空");
        assert!(
            !first.is_alphanumeric() && !first.is_whitespace(),
            "首字符应是零宽字符，实际是 {first:?}"
        );
        assert!(super::TRAY_ID.ends_with("catick"));
    }
}
