//! 调试工具：按窗口名找到 Catick 窗口，抓取一帧像素并打印采样点的 ARGB 值。
//!
//! 用法：先运行 `catick`，再执行 `cargo run --example probe`。
//! 用于在无法截图的环境（如 XWayland 下的 niri）验证贴图与 alpha 是否正确。

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    let win = find_window(&conn, root, "Catick")
        .ok_or("找不到名为 Catick 的窗口（catick 正在运行吗？）")?;

    let geom = conn.get_geometry(win)?.reply()?;
    let (w, h) = (geom.width as u32, geom.height as u32);
    println!("窗口 {win:#x}: {w}x{h} @ ({},{})", geom.x, geom.y);

    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, win, 0, 0, w as u16, h as u16, u32::MAX)?
        .reply()?;
    println!("图像 depth={} 数据长度={}", img.depth, img.data.len());

    let sample = |x: u32, y: u32| -> (u8, u8, u8, u8) {
        let i = ((y * w + x) * 4) as usize; // depth-32 ZPixmap：BGRX（小端）
        (img.data[i + 3], img.data[i + 2], img.data[i + 1], img.data[i])
    };
    for (label, x, y) in [
        ("左上边框", 1, 1),
        ("内部填充", w / 2, h / 2),
        ("左下角", 1, h - 2),
    ] {
        let (a, r, g, b) = sample(x, y);
        println!("{label} ({x},{y}): A={a} R={r} G={g} B={b}");
    }
    Ok(())
}

fn find_window(conn: &RustConnection, win: Window, name: &str) -> Option<Window> {
    let tree = conn.query_tree(win).ok()?.reply().ok()?;
    for child in tree.children {
        if window_name(conn, child).as_deref() == Some(name) {
            return Some(child);
        }
        if let Some(found) = find_window(conn, child, name) {
            return Some(found);
        }
    }
    None
}

fn window_name(conn: &RustConnection, win: Window) -> Option<String> {
    let atom = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let prop = conn
        .get_property(false, win, atom, AtomEnum::ANY, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    if prop.value.is_empty() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&prop.value)
            .trim_end_matches('\0')
            .to_string(),
    )
}
