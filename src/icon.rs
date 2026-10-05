//! 托盘图片图标：解码 PNG / GIF / JPEG，按需缩放渲染成托盘尺寸。
//!
//! 所有帧在加载时解码为预乘 ARGB 的 cairo surface 缓存在内存里，
//! 渲染时只做一次缩放 + 字节序转换（图标很小，开销可忽略）。

use cairo::{Context, Format, ImageSurface, Operator};
use std::path::Path;
use std::time::Duration;

/// 一帧：原尺寸的预乘 ARGB surface。
pub struct IconAnimation {
    frames: Vec<ImageSurface>,
    /// 每帧时长（GIF 自带；静态图为一帧无限长）
    delays: Vec<Duration>,
    /// 单帧总时长
    pub total: Duration,
}

impl IconAnimation {
    /// 当前时刻应显示的帧号（按各帧时长循环）。
    pub fn frame_at(&self, elapsed: Duration) -> usize {
        if self.frames.len() <= 1 || self.total.is_zero() {
            return 0;
        }
        let mut t = elapsed.as_millis() % self.total.as_millis().max(1);
        for (i, delay) in self.delays.iter().enumerate() {
            let ms = delay.as_millis().max(1);
            if t < ms {
                return i;
            }
            t -= ms;
        }
        0
    }

    /// 距下一帧还有多久（静态图返回 None）。
    pub fn next_frame_in(&self, elapsed: Duration) -> Option<Duration> {
        if self.frames.len() <= 1 || self.total.is_zero() {
            return None;
        }
        let current = self.frame_at(elapsed);
        let done: u128 = self.delays[..current]
            .iter()
            .map(|d| d.as_millis().max(1))
            .sum();
        let elapsed_in_frame = elapsed.as_millis() % self.total.as_millis().max(1) - done;
        let this = self.delays[current].as_millis().max(1);
        Some(Duration::from_millis(
            (this - elapsed_in_frame).max(1) as u64
        ))
    }

    /// 把第 `index` 帧缩放到 `size×size`，输出网络字节序 ARGB32（SNI 约定）。
    pub fn render(
        &self,
        index: usize,
        size: u32,
        dimmed: bool,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let src = &self.frames[index.min(self.frames.len() - 1)];
        let mut target = ImageSurface::create(Format::ARgb32, size as i32, size as i32)?;
        let cr = Context::new(&target)?;
        cr.set_operator(Operator::Clear);
        cr.paint()?;
        cr.set_operator(Operator::Source);
        if dimmed {
            // 暂停态整体变淡：画完再叠加一层乘性 alpha
            cr.push_group();
        }
        let (sw, sh) = (src.width() as f64, src.height() as f64);
        let scale = (size as f64 / sw).min(size as f64 / sh);
        let (dw, dh) = (sw * scale, sh * scale);
        cr.save()?;
        cr.translate((size as f64 - dw) / 2.0, (size as f64 - dh) / 2.0);
        cr.scale(scale, scale);
        cr.set_source_surface(src, 0.0, 0.0)?;
        cr.paint()?;
        cr.restore()?;
        if dimmed {
            cr.pop_group_to_source()?;
            cr.paint_with_alpha(0.55)?;
        }
        drop(cr);

        // cairo 原生是小端 BGRA；SNI 需要大端序的 ARGB 字节
        let stride = target.stride() as usize;
        let data = target.data()?;
        let row_bytes = size as usize * 4;
        let mut out = Vec::with_capacity(row_bytes * size as usize);
        for row in 0..size as usize {
            let start = row * stride;
            for px in data[start..start + row_bytes].chunks_exact(4) {
                let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Ok(out)
    }
}

/// 读取图片文件（按扩展名选择解码器）。
pub fn load(path: &Path) -> Result<IconAnimation, Box<dyn std::error::Error>> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => load_png(path),
        "gif" => load_gif(path),
        "jpg" | "jpeg" => load_jpeg(path),
        other => Err(format!("不支持的图片格式：{other}（支持 png / gif / jpg）").into()),
    }
}

fn load_png(path: &Path) -> Result<IconAnimation, Box<dyn std::error::Error>> {
    let mut file = std::fs::File::open(path)?;
    let surface = ImageSurface::create_from_png(&mut file)?;
    Ok(single_frame(surface))
}

fn load_jpeg(path: &Path) -> Result<IconAnimation, Box<dyn std::error::Error>> {
    let data = std::fs::read(path)?;
    let mut decoder = zune_jpeg::JpegDecoder::new(&data[..]);
    let pixels = decoder.decode()?;
    let (w, h) = decoder.dimensions().ok_or("JPEG 尺寸未知")?;
    let (w, h) = (w as u32, h as u32);
    // zune-jpeg 输出 RGB（每像素 3 字节）
    let mut argb = Vec::with_capacity(w as usize * h as usize * 4);
    for rgb in pixels.chunks_exact(3) {
        let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
        argb.extend_from_slice(&[b, g, r, 255]);
    }
    Ok(single_frame(surface_from_argb(w, h, argb)?))
}

fn load_gif(path: &Path) -> Result<IconAnimation, Box<dyn std::error::Error>> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let file = std::fs::File::open(path)?;
    let mut decoder = options.read_info(file)?;

    let (w, h) = (decoder.width() as u32, decoder.height() as u32);
    let mut canvas = vec![0u8; (w * h * 4) as usize];
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let mut prev_rect: Option<(u32, u32, u32, u32)> = None;
    let mut prev_dispose = gif::DisposalMethod::Keep;

    while let Some(frame) = decoder.read_next_frame()? {
        // 上一帧的处置方式：Background 需要清空其区域
        if prev_dispose == gif::DisposalMethod::Background
            && let Some((x, y, fw, fh)) = prev_rect
        {
            for row in y..(y + fh).min(h) {
                for col in x..(x + fw).min(w) {
                    let i = ((row * w + col) * 4) as usize;
                    canvas[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let (fx, fy) = (frame.left as u32, frame.top as u32);
        let (fw, fh) = (frame.width as u32, frame.height as u32);
        for row in 0..fh {
            for col in 0..fw {
                let (dx, dy) = (fx + col, fy + row);
                if dx >= w || dy >= h {
                    continue;
                }
                let src = ((row * fw + col) * 4) as usize;
                let dst = ((dy * w + dx) * 4) as usize;
                canvas[dst..dst + 4].copy_from_slice(&frame.buffer[src..src + 4]);
            }
        }
        frames.push(to_argb_surface(w, h, &canvas)?);
        // GIF 的 delay 单位是 1/100 秒；0 视为 100ms（浏览器惯例）
        delays.push(Duration::from_millis(
            if frame.delay == 0 {
                100
            } else {
                u32::from(frame.delay) * 10
            }
            .into(),
        ));
        prev_rect = Some((fx, fy, fw, fh));
        prev_dispose = frame.dispose;
    }

    if frames.is_empty() {
        return Err("GIF 没有可用帧".into());
    }
    let total: Duration = delays.iter().sum();
    Ok(IconAnimation {
        frames,
        delays,
        total,
    })
}

fn single_frame(surface: ImageSurface) -> IconAnimation {
    IconAnimation {
        frames: vec![surface],
        delays: vec![Duration::from_secs(1)],
        total: Duration::from_secs(1),
    }
}

/// RGBA（非预乘）画布 → 预乘 BGRA 的 cairo surface。
fn to_argb_surface(
    w: u32,
    h: u32,
    rgba: &[u8],
) -> Result<ImageSurface, Box<dyn std::error::Error>> {
    let mut argb = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = u32::from(px[3]);
        let premul = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
        argb.extend_from_slice(&[premul(px[2]), premul(px[1]), premul(px[0]), px[3]]);
    }
    surface_from_argb(w, h, argb)
}

fn surface_from_argb(
    w: u32,
    h: u32,
    argb: Vec<u8>,
) -> Result<ImageSurface, Box<dyn std::error::Error>> {
    Ok(ImageSurface::create_for_data(
        argb,
        Format::ARgb32,
        w as i32,
        h as i32,
        (w * 4) as i32,
    )?)
}
