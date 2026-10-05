//! 窗口后端抽象：X11（x11rb）与 Wayland（wlr-layer-shell）两个实现
//! 共享同一套事件循环与应用逻辑。
//!
//! 约定：`present` 的像素缓冲是紧排布、预乘 alpha 的 ARGB32（stride = w*4），
//! 即 cairo `Format::ARgb32` 的原生布局。

use crate::config::{Backend, Config};
use std::error::Error;
use std::os::fd::RawFd;

pub type OverlayResult<T> = Result<T, Box<dyn Error>>;

/// 修饰键状态。Wayland 的鼠标事件不携带修饰键（layer-shell 一般不拿键盘焦点），
/// 拿不到时保持默认值，相关功能走托盘菜单。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
}

/// 归一化后的输入/窗口事件。
#[derive(Debug, Clone, Copy)]
pub enum Input {
    /// 按键按下（X11 的 button：1 左键、3 右键、4/5 滚轮）
    ButtonPress {
        button: u8,
        modifiers: Modifiers,
        root_x: i32,
        root_y: i32,
    },
    ButtonRelease {
        button: u8,
    },
    /// 指针移动（拖动时用根坐标计算；Wayland 下为「局部坐标 + 窗口位置」）
    Motion { root_x: i32, root_y: i32 },
    /// 整帧需要重绘（Expose、被外部改变尺寸/缩放等）
    Redraw,
    /// 窗口已被显示服务器关闭，应退出
    Close,
}

/// 一块可贴图的悬浮窗口。
pub trait Overlay {
    /// 事件循环 poll 用的文件描述符（X11 socket / Wayland 连接）。
    /// Wayland 下会先做 `prepare_read`，必须在 poll 之后调用 `drain_events(readable)`。
    fn poll_fd(&mut self) -> RawFd;
    /// 处理已排队事件；`readable` 表示 poll 报告该 fd 可读。
    fn drain_events(&mut self, readable: bool) -> Vec<Input>;
    /// 把已发出的请求冲刷到显示服务器。
    fn flush(&mut self) -> OverlayResult<()>;
    /// 设置窗口位置与大小（逻辑坐标）；宽高与下一次 `present` 的物理缓冲换算关系见 `scale()`。
    fn set_geometry(&mut self, x: i32, y: i32, w: u32, h: u32) -> OverlayResult<()>;
    /// 当前 (x, y, w, h)（逻辑坐标）。
    fn geometry(&self) -> (i32, i32, u32, u32);
    /// 贴一帧；`w`/`h` 是缓冲的物理像素尺寸（X11 下与逻辑尺寸相同）。
    fn present(&mut self, buf: &[u8], w: u32, h: u32) -> OverlayResult<()>;
    /// 鼠标穿透：开启后窗口不接收指针事件。
    fn set_passthrough(&mut self, on: bool) -> OverlayResult<()>;
    /// 逻辑屏幕尺寸，用于默认位置与边界约束。
    fn screen_size(&self) -> (u32, u32);
    /// 物理像素 / 逻辑像素（X11 恒为 1.0；Wayland 支持分数缩放）。
    fn scale(&self) -> f64 {
        1.0
    }
    /// 后端名称（日志用）。
    fn name(&self) -> &'static str;
}

/// 按配置选择后端；`auto` 时优先 Wayland（有 layer-shell 可用），否则 X11。
pub fn create(cfg: &Config) -> OverlayResult<Box<dyn Overlay>> {
    match cfg.backend {
        Backend::X11 => Ok(Box::new(crate::x11::X11Overlay::new()?)),
        Backend::Wayland => crate::wayland::create(),
        Backend::Auto => {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                match crate::wayland::create() {
                    Ok(ov) => return Ok(ov),
                    Err(err) => eprintln!("catick: Wayland 后端不可用（{err}），回退 X11"),
                }
            }
            Ok(Box::new(crate::x11::X11Overlay::new()?))
        }
    }
}
