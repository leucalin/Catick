//! 调试工具：用 `zwlr_virtual_pointer_manager_v1` 注入指针事件，
//! 相当于 Wayland 版的 XTEST（需要合成器支持该协议，niri/sway 支持）。
//!
//! 用法（坐标是合成器的逻辑布局坐标）：
//!   cargo run --example winject -- abs X Y [EXTENT_X EXTENT_Y]
//!   cargo run --example winject -- drag X1 Y1 X2 Y2 [EXTENT_X EXTENT_Y]
//!   cargo run --example winject -- click N

use std::time::Duration;
use wayland_client::EventQueue;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_pointer, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use wl_pointer::ButtonState;

#[derive(Default)]
struct State;

macro_rules! ignore {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

ignore!(ZwlrVirtualPointerManagerV1);
ignore!(ZwlrVirtualPointerV1);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let mut state = State;
    let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ())?;
    let vp = manager.create_virtual_pointer(None, &qh, ());
    conn.flush()?;

    let extent = |args: &[String], from: usize| -> (u32, u32) {
        match (args.get(from), args.get(from + 1)) {
            (Some(x), Some(y)) => (x.parse().unwrap_or(1920), y.parse().unwrap_or(1080)),
            _ => (1920, 1080),
        }
    };

    match args.first().map(String::as_str) {
        Some("abs") => {
            let (x, y): (u32, u32) = (args[1].parse()?, args[2].parse()?);
            let (ex, ey) = extent(&args, 3);
            absolute(&conn, &mut queue, &mut state, &vp, x, y, ex, ey)?;
        }
        Some("click") => {
            let code = match args[1].parse::<u32>()? {
                1 => 0x110,
                2 => 0x112,
                _ => 0x111,
            };
            button(
                &conn,
                &mut queue,
                &mut state,
                &vp,
                code,
                ButtonState::Pressed,
            )?;
            std::thread::sleep(Duration::from_millis(40));
            button(
                &conn,
                &mut queue,
                &mut state,
                &vp,
                code,
                ButtonState::Released,
            )?;
        }
        Some("drag") => {
            let (x1, y1): (i32, i32) = (args[1].parse()?, args[2].parse()?);
            let (x2, y2): (i32, i32) = (args[3].parse()?, args[4].parse()?);
            let (ex, ey) = extent(&args, 5);
            absolute(
                &conn, &mut queue, &mut state, &vp, x1 as u32, y1 as u32, ex, ey,
            )?;
            std::thread::sleep(Duration::from_millis(60));
            button(
                &conn,
                &mut queue,
                &mut state,
                &vp,
                0x110,
                ButtonState::Pressed,
            )?;
            std::thread::sleep(Duration::from_millis(60));
            // 分步相对移动，模拟真实拖动轨迹
            let steps = 20;
            for i in 1..=steps {
                let dx = (x2 - x1) / steps + if i == steps { (x2 - x1) % steps } else { 0 };
                let dy = (y2 - y1) / steps + if i == steps { (y2 - y1) % steps } else { 0 };
                vp.motion(0, dx as f64, dy as f64);
                vp.frame();
                conn.flush()?;
                queue.roundtrip(&mut state)?;
                std::thread::sleep(Duration::from_millis(16));
            }
            std::thread::sleep(Duration::from_millis(60));
            button(
                &conn,
                &mut queue,
                &mut state,
                &vp,
                0x110,
                ButtonState::Released,
            )?;
        }
        Some("rel") => {
            // 不按键的相对移动（对照实验用）
            let (dx, dy): (f64, f64) = (args[1].parse()?, args[2].parse()?);
            let steps: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
            for _ in 0..steps {
                vp.motion(0, dx, dy);
                vp.frame();
                conn.flush()?;
                queue.roundtrip(&mut state)?;
                std::thread::sleep(Duration::from_millis(16));
            }
        }
        _ => {
            eprintln!(
                "用法: winject abs X Y [EX EY] | rel DX DY [N] | drag X1 Y1 X2 Y2 [EX EY] | click N"
            );
            std::process::exit(2);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn absolute(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    vp: &ZwlrVirtualPointerV1,
    x: u32,
    y: u32,
    extent_x: u32,
    extent_y: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    vp.motion_absolute(0, x, y, extent_x, extent_y);
    vp.frame();
    conn.flush()?;
    queue.roundtrip(state)?;
    Ok(())
}

fn button(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    vp: &ZwlrVirtualPointerV1,
    code: u32,
    button_state: ButtonState,
) -> Result<(), Box<dyn std::error::Error>> {
    vp.button(0, code, button_state);
    vp.frame();
    conn.flush()?;
    queue.roundtrip(state)?;
    Ok(())
}
