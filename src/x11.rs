//! X11 后端：depth-32 ARGB 的 override-redirect 窗口，cairo 帧经 `put_image` 贴到窗口上。
//!
//! 适用于 X11 会话，以及其他合成器的 XWayland；niri 的 xwayland-satellite
//! 不尊重 override-redirect 摆放，那种环境请使用 Wayland 后端。

use crate::overlay::{Input, Modifiers, Overlay, OverlayResult};
use std::os::fd::{AsRawFd, RawFd};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{ClipOrdering, ConnectionExt as _, *};
use x11rb::rust_connection::RustConnection;

/// 单次 put_image 请求的最大字节数（保守值，避免触发 BigRequests）。
const MAX_REQUEST_BYTES: usize = 262_000;

pub struct X11Overlay {
    conn: RustConnection,
    win: Window,
    gc: Gcontext,
    depth: u8,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    screen_w: u32,
    screen_h: u32,
    has_shape: bool,
    passthrough: bool,
}

impl X11Overlay {
    pub fn new() -> OverlayResult<Self> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let (screen_w, screen_h) = (
            screen.width_in_pixels as u32,
            screen.height_in_pixels as u32,
        );

        // 32 位 ARGB visual 才有逐像素透明；找不到时退化为不透明窗口
        let (visual, depth) = match find_argb_visual(&conn, screen_num) {
            Some(v) => v,
            None => {
                eprintln!("catick: 未找到 32 位 ARGB visual，窗口将不透明");
                (screen.root_visual, screen.root_depth)
            }
        };

        let colormap = if visual != screen.root_visual {
            let cmap = conn.generate_id()?;
            conn.create_colormap(ColormapAlloc::NONE, cmap, root, visual)?;
            Some(cmap)
        } else {
            None
        };

        let mut aux = CreateWindowAux::new()
            .background_pixel(0)
            .border_pixel(0)
            .override_redirect(1)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION,
            );
        if let Some(cmap) = colormap {
            aux = aux.colormap(cmap);
        }

        let win = conn.generate_id()?;
        conn.create_window(
            depth,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &aux,
        )?;

        // 便于调试识别与窗口规则（X11 下 niri 的 app-id 取自 WM_CLASS）
        set_prop(&conn, win, b"WM_NAME", b"Catick")?;
        set_prop(&conn, win, b"_NET_WM_NAME", b"Catick")?;
        set_prop(&conn, win, b"WM_CLASS", b"catick\0Catick")?;

        // GC 的 depth 取自 drawable，必须用 depth-32 的窗口创建，否则 put_image 报 BadMatch
        let gc = conn.generate_id()?;
        conn.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;

        let has_shape = conn
            .extension_information(shape::X11_EXTENSION_NAME)?
            .is_some();
        if !has_shape {
            eprintln!("catick: 服务器不支持 SHAPE 扩展，鼠标穿透不可用");
        }

        conn.map_window(win)?;
        conn.flush()?;

        Ok(Self {
            conn,
            win,
            gc,
            depth,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            screen_w,
            screen_h,
            has_shape,
            passthrough: false,
        })
    }
}

impl Overlay for X11Overlay {
    fn poll_fd(&mut self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    fn drain_events(&mut self, _readable: bool) -> Vec<Input> {
        let mut out = Vec::new();
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::ButtonPress(e))) => out.push(Input::ButtonPress {
                    button: e.detail,
                    modifiers: modifiers_from(e.state),
                    root_x: e.root_x as i32,
                    root_y: e.root_y as i32,
                }),
                Ok(Some(Event::ButtonRelease(e))) => {
                    out.push(Input::ButtonRelease { button: e.detail })
                }
                Ok(Some(Event::MotionNotify(e))) => out.push(Input::Motion {
                    root_x: e.root_x as i32,
                    root_y: e.root_y as i32,
                }),
                Ok(Some(Event::Expose(e))) => {
                    if e.count == 0 {
                        out.push(Input::Redraw);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(err) => {
                    eprintln!("catick: X11 事件读取失败: {err}");
                    break;
                }
            }
        }
        out
    }

    fn flush(&mut self) -> OverlayResult<()> {
        self.conn.flush()?;
        Ok(())
    }

    fn set_geometry(&mut self, x: i32, y: i32, w: u32, h: u32) -> OverlayResult<()> {
        self.conn.configure_window(
            self.win,
            &ConfigureWindowAux::new().x(x).y(y).width(w).height(h),
        )?;
        self.conn.flush()?;
        self.x = x;
        self.y = y;
        self.w = w;
        self.h = h;
        Ok(())
    }

    fn geometry(&self) -> (i32, i32, u32, u32) {
        (self.x, self.y, self.w, self.h)
    }

    fn present(&mut self, buf: &[u8], w: u32, h: u32) -> OverlayResult<()> {
        let stride = w as usize * 4;
        if buf.len() < stride * h as usize {
            return Err(format!("帧缓冲尺寸不足: {} < {}x{}x4", buf.len(), w, h).into());
        }
        // 分带发送，单请求不超过 MAX_REQUEST_BYTES
        let band_rows = (MAX_REQUEST_BYTES / stride.max(1)).max(1) as u32;
        let mut y = 0u32;
        while y < h {
            let rows = band_rows.min(h - y);
            let start = y as usize * stride;
            let end = start + rows as usize * stride;
            self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.win,
                self.gc,
                w as u16,
                rows as u16,
                0,
                y as i16,
                0,
                self.depth,
                &buf[start..end],
            )?;
            y += rows;
        }
        self.conn.flush()?;
        Ok(())
    }

    fn set_passthrough(&mut self, on: bool) -> OverlayResult<()> {
        if self.passthrough == on {
            return Ok(());
        }
        if self.has_shape {
            if on {
                // 空输入区域：指针事件穿透到下层窗口
                self.conn.shape_rectangles(
                    SO::SET,
                    SK::INPUT,
                    ClipOrdering::UNSORTED,
                    self.win,
                    0,
                    0,
                    &[],
                )?;
            } else {
                let full = Rectangle {
                    x: 0,
                    y: 0,
                    width: self.w as u16,
                    height: self.h as u16,
                };
                self.conn.shape_rectangles(
                    SO::SET,
                    SK::INPUT,
                    ClipOrdering::UNSORTED,
                    self.win,
                    0,
                    0,
                    &[full],
                )?;
            }
            self.conn.flush()?;
        }
        self.passthrough = on;
        Ok(())
    }

    fn screen_size(&self) -> (u32, u32) {
        (self.screen_w, self.screen_h)
    }

    fn name(&self) -> &'static str {
        "x11"
    }
}

/// 从按键状态掩码提取我们关心的修饰键。
fn modifiers_from(state: KeyButMask) -> Modifiers {
    Modifiers {
        shift: state.contains(KeyButMask::SHIFT),
        ctrl: state.contains(KeyButMask::CONTROL),
    }
}

/// 写入 STRING 类型的窗口属性。
fn set_prop(
    conn: &RustConnection,
    win: Window,
    name: &[u8],
    value: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let atom = conn.intern_atom(false, name)?.reply()?.atom;
    conn.change_property(
        PropMode::REPLACE,
        win,
        atom,
        AtomEnum::STRING,
        8,
        value.len() as u32,
        value,
    )?;
    Ok(())
}

/// 找到 depth-32 的 TrueColor visual（透明窗口所需）。
fn find_argb_visual(conn: &RustConnection, screen_num: usize) -> Option<(Visualid, u8)> {
    let screen = &conn.setup().roots[screen_num];
    for depth in &screen.allowed_depths {
        if depth.depth == 32 {
            for visual in &depth.visuals {
                if visual.class == VisualClass::TRUE_COLOR {
                    return Some((visual.visual_id, depth.depth));
                }
            }
        }
    }
    None
}
