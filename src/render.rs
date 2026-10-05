//! cairo 渲染：把一行文字绘制成预乘 ARGB32 缓冲（stride = w*4，与 X11/Wayland 直接兼容）。
//!
//! 窗口尺寸用「模板字符串」测量（如 `88:88:88`），保证秒数变化时窗口宽度不抖动。

use crate::config::Config;
use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Operator};
use std::path::Path;

/// 文字周围留白（像素），避免抗锯齿边缘被裁切。
const PADDING: f64 = 6.0;

/// 尺寸测量用的模板：所有计时模式都是等宽的 `HH:MM:SS`。
pub const TEMPLATE: &str = "88:88:88";

/// 渲染样式（由配置派生，编辑模式下会被就地调整后回写配置）。
#[derive(Debug, Clone)]
pub struct Style {
    pub family: String,
    pub size: f64,
    pub bold: bool,
    pub color: (f64, f64, f64),
    /// 0.0 ~ 1.0
    pub opacity: f64,
}

impl Style {
    pub fn from_config(cfg: &Config) -> Self {
        Style {
            family: cfg.font_family.clone(),
            size: cfg.font_size,
            bold: cfg.bold,
            color: cfg.color_rgb(),
            opacity: cfg.opacity.clamp(0.0, 1.0),
        }
    }
}

/// 一帧像素：预乘 ARGB32，紧排布（stride = width*4）。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// 按模板字符串测量窗口尺寸。
pub fn measure(style: &Style, template: &str) -> (u32, u32) {
    measure_inner(style, template).unwrap_or((16, 16))
}

fn measure_inner(style: &Style, template: &str) -> Option<(u32, u32)> {
    let surface = ImageSurface::create(Format::ARgb32, 1, 1).ok()?;
    let cr = Context::new(&surface).ok()?;
    apply_font(&cr, style);
    let te = cr.text_extents(template).ok()?;
    let fe = cr.font_extents().ok()?;
    let w = (te.x_advance().ceil() + 2.0 * PADDING).max(1.0) as u32;
    let h = (fe.height().ceil() + 2.0 * PADDING).max(1.0) as u32;
    Some((w, h))
}

/// 渲染一帧：整幅清为透明，文字在正中显示；`edit` 时叠加编辑模式指示底。
pub fn render(
    style: &Style,
    text: &str,
    width: u32,
    height: u32,
    edit: bool,
) -> Result<Frame, Box<dyn std::error::Error>> {
    let mut surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)?;
    let cr = Context::new(&surface)?;

    // 全透明底（Clear 会把 alpha 也清零；之后必须复位为 Source）
    cr.set_operator(Operator::Clear);
    cr.paint()?;
    cr.set_operator(Operator::Source);

    if edit {
        // 编辑模式：暗底 + 细边框，提示当前可拖动/滚轮调整
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
        cr.rectangle(1.0, 1.0, width as f64 - 2.0, height as f64 - 2.0);
        cr.fill()?;
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.55);
        cr.set_line_width(1.0);
        cr.rectangle(1.5, 1.5, width as f64 - 3.0, height as f64 - 3.0);
        cr.stroke()?;
    }

    apply_font(&cr, style);
    let (r, g, b) = style.color;
    cr.set_source_rgba(r, g, b, style.opacity);

    // 以「字体整体高度」定垂直中线、以「advance 宽度」定水平中线
    let fe = cr.font_extents()?;
    let te = cr.text_extents(text)?;
    let x = (width as f64 - te.x_advance()) / 2.0 - te.x_bearing();
    let y = (height as f64 - fe.height()) / 2.0 + fe.ascent();
    cr.move_to(x, y);
    cr.show_text(text)?;

    // cairo 的 Context 持有 surface 引用，取像素前必须先释放
    drop(cr);

    // 拷成紧排布缓冲（ARgb32 的 stride 本来就是 w*4，这里仍按行防御性拷贝）
    let stride = surface.stride() as usize;
    let data = surface.data()?;
    let row_bytes = width as usize * 4;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        pixels.extend_from_slice(&data[start..start + row_bytes]);
    }
    Ok(Frame {
        width,
        height,
        pixels,
    })
}

/// 把一帧写成 PNG（调试与无显示环境验证用）。
pub fn dump_png(frame: Frame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stride = (frame.width * 4) as i32;
    let surface = ImageSurface::create_for_data(
        frame.pixels,
        Format::ARgb32,
        frame.width as i32,
        frame.height as i32,
        stride,
    )?;
    let mut file = std::fs::File::create(path)?;
    surface.write_to_png(&mut file)?;
    Ok(())
}

fn apply_font(cr: &Context, style: &Style) {
    let weight = if style.bold {
        FontWeight::Bold
    } else {
        FontWeight::Normal
    };
    cr.select_font_face(&style.family, FontSlant::Normal, weight);
    cr.set_font_size(style.size);
}
