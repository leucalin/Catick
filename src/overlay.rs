//! 窗口后端抽象：X11（x11rb）与 Wayland（wlr-layer-shell）两个实现
//! 共享同一套事件循环与应用逻辑。
//!
//! 约定：`present` 的像素缓冲是紧排布、预乘 alpha 的 ARGB32（stride = w*4），
//! 即 cairo `Format::ARgb32` 的原生布局。

use crate::config::{Backend, Config};
use std::error::Error;
use std::os::fd::RawFd;

pub type OverlayResult<T> = Result<T, Box<dyn Error>>;

/// 归一化后的输入/窗口事件。
#[derive(Debug, Clone, Copy)]
pub enum Input {
    /// 按键按下（X11 的 button：1 左键、4/5 滚轮）
    ButtonPress { button: u8, root_x: i32, root_y: i32 },
    ButtonRelease { button: u8, root_x: i32, root_y: i32 },
    /// 指针移动（拖动时用根坐标计算）
    Motion { root_x: i32, root_y: i32 },
    /// 整帧需要重绘（Expose、被外部改变尺寸等）
    Redraw,
}

/// 一块可贴图的悬浮窗口。
pub trait Overlay {
    /// 事件循环 poll 用的文件描述符（X11 socket / Wayland 连接）。
    fn event_fd(&self) -> RawFd;
    /// 非阻塞读取并清空当前全部事件。
    fn drain_events(&mut self) -> Vec<Input>;
    /// 把已发出的请求冲刷到显示服务器。
    fn flush(&mut self) -> OverlayResult<()>;
    /// 设置窗口位置与大小；宽高必须与下一次 `present` 的缓冲一致。
    fn set_geometry(&mut self, x: i32, y: i32, w: u32, h: u32) -> OverlayResult<()>;
    /// 当前 (x, y, w, h)。
    fn geometry(&self) -> (i32, i32, u32, u32);
    /// 按当前几何贴一帧。
    fn present(&mut self, buf: &[u8]) -> OverlayResult<()>;
    /// 鼠标穿透：开启后窗口不接收指针事件。
    fn set_passthrough(&mut self, on: bool) -> OverlayResult<()>;
    /// 逻辑屏幕尺寸，用于默认位置与边界约束。
    fn screen_size(&self) -> (u32, u32);
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
