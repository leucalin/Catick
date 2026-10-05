use x11rb::connection::Connection;
use x11rb::errors::ReplyOrIdError;
use x11rb::protocol::xproto::*;
use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::rust_connection::RustConnection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None).unwrap();
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let (visual_id, depth) = find_argb_visual(&conn, screen_num).unwrap();

    let colormap = conn.generate_id()?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual_id);

    let win_id = conn.generate_id()?;
    conn.create_window(
        depth,
        win_id,
        root,
        0,
        0,
        100,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        visual_id,
        &&CreateWindowAux::new().background_pixel(0).border_pixel(0).colormap(colormap).override_redirect(1).event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
    )?;
    conn.map_window(win_id)?;
    conn.flush()?;
    loop {
        println!("Event: {:?}", conn.wait_for_event()?);
    }
}

fn find_argb_visual(conn: &RustConnection, screen_num: usize) -> Option<(Visualid, u8)> {
    let screen = &conn.setup().roots[screen_num];
    
    // 遍历所有允许的深度（allowed_depths）
    for depth in &screen.allowed_depths {
        // 32 位深度通常对应 ARGB 透明视觉
        if depth.depth == 32 {
            // 遍历该深度下的所有视觉
            for visual in &depth.visuals {
                // 返回第一个找到的 32 位视觉的 ID 和深度
                return Some((visual.visual_id, depth.depth));
            }
        }
    }
    None
}

