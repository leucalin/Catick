//! Wayland 后端：wlr-layer-shell 的 overlay 层窗口（水印式悬浮）。
//!
//! 这是占位实现；真正的 layer-shell 支持会在后续步骤补齐，
//! 此前 `auto` 模式会回退到 X11 后端。

use crate::overlay::{Overlay, OverlayResult};

/// 创建 Wayland 悬浮层；未实现前返回错误，由调用方决定是否回退。
pub fn create() -> OverlayResult<Box<dyn Overlay>> {
    Err("Wayland 后端尚未实现".into())
}
