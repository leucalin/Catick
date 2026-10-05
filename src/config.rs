//! 配置：默认值、`~/.config/catick/config.toml` 读写、时长/颜色解析。
//!
//! 配置全部字段都有默认值，缺失文件或字段时使用默认值启动；
//! 解析失败的文件会被备份为 `config.toml.bak`，避免后续保存覆盖用户数据。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// 计时模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 显示当前时间
    Clock,
    /// 倒计时
    #[default]
    Countdown,
    /// 秒表（累计计时）
    Stopwatch,
    /// 番茄钟：工作 / 短休 / 长休 自动流转
    Pomodoro,
}

/// 窗口后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// 有 Wayland 会话时用 layer-shell，否则用 X11
    #[default]
    Auto,
    X11,
    Wayland,
}

/// 番茄钟各阶段时长与循环数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Pomodoro {
    /// 工作时长，如 "25m"
    pub work: String,
    /// 短休息时长
    pub short_break: String,
    /// 长休息时长
    pub long_break: String,
    /// 每完成多少个工作阶段后进入长休息
    pub cycles: u32,
}

impl Default for Pomodoro {
    fn default() -> Self {
        Pomodoro {
            work: "25m".into(),
            short_break: "5m".into(),
            long_break: "15m".into(),
            cycles: 4,
        }
    }
}

/// 窗口左上角在屏幕上的坐标（像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    /// 倒计时默认时长，如 "25m" / "90s" / "1h30m" / "25:00"
    pub duration: String,
    pub font_family: String,
    pub font_size: f64,
    pub bold: bool,
    /// 文字颜色，`#rrggbb`
    pub color: String,
    /// 整体不透明度 0.0 ~ 1.0
    pub opacity: f64,
    /// 时钟模式使用 24 小时制
    pub clock_24h: bool,
    /// 鼠标穿透（水印式，不拦截桌面操作）
    pub click_through: bool,
    /// 启动即进入编辑模式
    pub edit_on_start: bool,
    /// 启用系统托盘（StatusNotifierItem）
    pub tray: bool,
    pub backend: Backend,
    /// 窗口位置；None 表示居中偏上
    pub position: Option<Position>,
    /// 倒计时结束时执行的命令（经 `sh -c`），None 表示不执行
    pub on_finish_cmd: Option<String>,
    pub pomodoro: Pomodoro,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Countdown,
            duration: "25m".into(),
            font_family: "monospace".into(),
            font_size: 72.0,
            bold: true,
            color: "#ffffff".into(),
            opacity: 1.0,
            clock_24h: true,
            click_through: true,
            edit_on_start: false,
            tray: true,
            backend: Backend::Auto,
            position: None,
            on_finish_cmd: Some("notify-send Catick \"Time's up!\"".into()),
            pomodoro: Pomodoro::default(),
        }
    }
}

impl Config {
    /// 配置文件的路径：`$XDG_CONFIG_HOME/catick/config.toml`（回退 `~/.config`）。
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("catick").join("config.toml"))
    }

    /// 读取配置；文件缺失或字段缺失时用默认值补齐。
    pub fn load() -> Config {
        let Some(path) = Self::path() else {
            return Config::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Config::default();
        };
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => cfg,
            Err(err) => {
                // 保留用户原文件，避免之后的自动保存把内容覆盖掉
                let backup = path.with_extension("toml.bak");
                let _ = std::fs::rename(&path, &backup);
                eprintln!(
                    "catick: {} 解析失败（{err}），已备份为 {}",
                    path.display(),
                    backup.display()
                );
                Config::default()
            }
        }
    }

    /// 保存配置；失败只打日志，不影响运行。
    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent()
            && let Err(err) = std::fs::create_dir_all(dir)
        {
            eprintln!("catick: 无法创建配置目录 {}: {err}", dir.display());
            return;
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&path, text) {
                    eprintln!("catick: 无法写入 {}: {err}", path.display());
                }
            }
            Err(err) => eprintln!("catick: 配置序列化失败: {err}"),
        }
    }

    /// 倒计时时长（解析失败回退 25 分钟）。
    pub fn duration(&self) -> Duration {
        parse_duration(&self.duration).unwrap_or(Duration::from_secs(25 * 60))
    }

    /// 文字颜色 RGB（解析失败回退白色）。
    pub fn color_rgb(&self) -> (f64, f64, f64) {
        parse_color(&self.color).unwrap_or((1.0, 1.0, 1.0))
    }
}

/// 解析时长：`90s` / `25m` / `1h30m` / `1.5m` / `25`（裸数字按分钟）/ `25:00` / `1:02:03`。
pub fn parse_duration(input: &str) -> Option<Duration> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        let mut secs = 0u64;
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() > 3 {
            return None;
        }
        for part in parts {
            secs = secs * 60 + part.trim().parse::<u64>().ok()?;
        }
        return Some(Duration::from_secs(secs));
    }
    let mut total = 0f64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let value: f64 = num.parse().ok()?;
        num.clear();
        match c.to_ascii_lowercase() {
            'h' => total += value * 3600.0,
            'm' => total += value * 60.0,
            's' => total += value,
            _ => return None,
        }
    }
    if !num.is_empty() {
        // 末尾裸数字按分钟
        total += num.parse::<f64>().ok()? * 60.0;
    }
    if total > 0.0 {
        Some(Duration::from_secs_f64(total))
    } else {
        None
    }
}

/// 解析颜色：`#rgb` / `#rrggbb`（`#` 可省略），返回 0.0~1.0 的 RGB。
pub fn parse_color(input: &str) -> Option<(f64, f64, f64)> {
    let s = input.trim().trim_start_matches('#');
    let expand = |v: u8| v as f64 / 255.0;
    match s.len() {
        3 => {
            let v = u16::from_str_radix(s, 16).ok()?;
            let (r, g, b) = (
                ((v >> 8) & 0xf) as u8,
                ((v >> 4) & 0xf) as u8,
                (v & 0xf) as u8,
            );
            Some((expand(r * 16 + r), expand(g * 16 + g), expand(b * 16 + b)))
        }
        6 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            Some((
                expand(((v >> 16) & 0xff) as u8),
                expand(((v >> 8) & 0xff) as u8),
                expand((v & 0xff) as u8),
            ))
        }
        _ => None,
    }
}

/// 托盘菜单提供的颜色预设（名称, `#rrggbb`）。
pub const COLOR_PRESETS: [(&str, &str); 12] = [
    ("白色", "#ffffff"),
    ("黑色", "#000000"),
    ("灰色", "#9ca3af"),
    ("红色", "#ff5555"),
    ("橙色", "#ff9f43"),
    ("黄色", "#f9ca45"),
    ("绿色", "#2ecc71"),
    ("青色", "#22d3ee"),
    ("蓝色", "#4f8cff"),
    ("紫色", "#a78bfa"),
    ("粉色", "#ff7ab6"),
    ("棕色", "#b08968"),
];

/// 托盘字体候选：通用族 + 常见等宽/数字字体 ∩ 系统已安装（经 `fc-list`）。
/// 当前配置的字体总会出现在列表里。
pub fn available_fonts(current: &str) -> Vec<String> {
    const NAMED: [&str; 17] = [
        "JetBrainsMono Nerd Font",
        "JetBrains Mono",
        "Fira Code",
        "Hack",
        "DejaVu Sans Mono",
        "Noto Sans Mono",
        "Noto Sans",
        "Ubuntu Mono",
        "Source Code Pro",
        "Cascadia Code",
        "Iosevka",
        "Inter",
        "Lato",
        "Shure Tech Mono",
        "DS-Digital",
        "Digital-7",
        "Orbitron",
    ];
    let installed = installed_families();
    let mut fonts: Vec<String> = ["monospace", "sans-serif", "serif"]
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    fonts.extend(
        NAMED
            .iter()
            .filter(|name| installed.iter().any(|f| f.eq_ignore_ascii_case(name)))
            .map(|name| (*name).to_string()),
    );
    if !fonts.iter().any(|f| f == current) {
        fonts.insert(0, current.to_string());
    }
    fonts
}

/// 系统已安装的字体族列表（`fc-list : family`）。
fn installed_families() -> Vec<String> {
    // 注意：必须是两个参数（`: family`），否则 fc-list 按路径格式输出
    let Ok(output) = std::process::Command::new("fc-list")
        .args([":", "family"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .split(['\n', ','])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_forms() {
        let secs = |s: &str| parse_duration(s).map(|d| d.as_secs());
        assert_eq!(secs("90s"), Some(90));
        assert_eq!(secs("25m"), Some(25 * 60));
        assert_eq!(secs("1h30m"), Some(90 * 60));
        assert_eq!(secs("1.5m"), Some(90));
        assert_eq!(secs("25"), Some(25 * 60));
        assert_eq!(secs("25:00"), Some(25 * 60));
        assert_eq!(secs("1:02:03"), Some(3723));
        assert_eq!(secs(""), None);
        assert_eq!(secs("abc"), None);
        assert_eq!(secs("0"), None);
    }

    #[test]
    fn font_list_always_contains_current() {
        let fonts = available_fonts("monospace");
        assert!(fonts.iter().any(|f| f == "monospace"));
        let fonts = available_fonts("Some Custom Font");
        assert_eq!(fonts.first().map(String::as_str), Some("Some Custom Font"));
    }

    #[test]
    fn color_forms() {
        assert_eq!(parse_color("#ffffff"), Some((1.0, 1.0, 1.0)));
        assert_eq!(parse_color("000000"), Some((0.0, 0.0, 0.0)));
        assert_eq!(parse_color("#f00"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_color("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_color("bogus"), None);
    }
}
