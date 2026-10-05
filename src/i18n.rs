//! 界面文案（中文 / English）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// 按 $LANG 自动选择
    #[default]
    Auto,
    Zh,
    En,
}

impl Lang {
    /// `auto` 时按 `$LANG` 判断：`zh*` 用中文，其余用英文。
    pub fn resolved(self) -> Lang {
        match self {
            Lang::Auto => {
                let zh = std::env::var("LANG")
                    .map(|l| l.to_lowercase().starts_with("zh"))
                    .unwrap_or(false);
                if zh { Lang::Zh } else { Lang::En }
            }
            other => other,
        }
    }

    pub fn t(self) -> &'static Strings {
        match self.resolved() {
            Lang::En => &EN,
            _ => &ZH,
        }
    }

    /// 语言菜单里的名字（用各自的语言书写）。
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English",
            _ => "中文",
        }
    }
}

pub struct Strings {
    pub pause: &'static str,
    pub resume: &'static str,
    pub reset: &'static str,
    pub mode: &'static str,
    pub clock: &'static str,
    pub countdown: &'static str,
    pub stopwatch: &'static str,
    pub pomodoro: &'static str,
    pub presets: &'static str,
    pub minute: &'static str,
    pub edit_mode: &'static str,
    pub click_through: &'static str,
    pub clock_24h: &'static str,
    pub font: &'static str,
    pub font_more: &'static str,
    pub color: &'static str,
    pub font_size: &'static str,
    pub increase: &'static str,
    pub decrease: &'static str,
    pub opacity: &'static str,
    pub raise: &'static str,
    pub lower: &'static str,
    pub tray_icon: &'static str,
    pub icon_ring: &'static str,
    pub icon_image: &'static str,
    pub settings: &'static str,
    pub open_config_dir: &'static str,
    pub language: &'static str,
    pub quit: &'static str,
    pub paused: &'static str,
    pub phase_work: &'static str,
    pub phase_short: &'static str,
    pub phase_long: &'static str,
}

pub static ZH: Strings = Strings {
    pause: "暂停",
    resume: "继续",
    reset: "重置",
    mode: "模式",
    clock: "时钟",
    countdown: "倒计时",
    stopwatch: "秒表",
    pomodoro: "番茄钟",
    presets: "快捷预设",
    minute: "分钟",
    edit_mode: "编辑模式",
    click_through: "鼠标穿透",
    clock_24h: "24 小时制",
    font: "字体",
    font_more: "浏览字体文件…",
    color: "颜色",
    font_size: "字号",
    increase: "增大",
    decrease: "减小",
    opacity: "透明度",
    raise: "提高",
    lower: "降低",
    settings: "设置",
    open_config_dir: "打开配置目录",
    tray_icon: "托盘图标",
    icon_ring: "进度环",
    icon_image: "图片…",
    language: "语言",
    quit: "退出",
    paused: "已暂停",
    phase_work: "工作",
    phase_short: "短休",
    phase_long: "长休",
};

pub static EN: Strings = Strings {
    pause: "Pause",
    resume: "Resume",
    reset: "Reset",
    mode: "Mode",
    clock: "Clock",
    countdown: "Countdown",
    stopwatch: "Stopwatch",
    pomodoro: "Pomodoro",
    presets: "Quick presets",
    minute: "min",
    edit_mode: "Edit mode",
    click_through: "Click-through",
    clock_24h: "24-hour clock",
    font: "Font",
    font_more: "Browse font file…",
    color: "Color",
    font_size: "Font size",
    increase: "Increase",
    decrease: "Decrease",
    opacity: "Opacity",
    raise: "Raise",
    lower: "Lower",
    settings: "Settings",
    open_config_dir: "Open config folder",
    tray_icon: "Tray icon",
    icon_ring: "Progress ring",
    icon_image: "Image…",
    language: "Language",
    quit: "Quit",
    paused: "Paused",
    phase_work: "Work",
    phase_short: "Short break",
    phase_long: "Long break",
};
