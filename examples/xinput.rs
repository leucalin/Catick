//! 调试工具：用 XTEST 注入指针事件，在缺少 xdotool 的环境下做交互测试。
//!
//! 用法（先运行 catick --interactive）：
//!   cargo run --example xinput -- move X Y
//!   cargo run --example xinput -- click N          # 1 左键 / 2 中键 / 3 右键
//!   cargo run --example xinput -- scroll N         # 正数向上滚 N 格，负数向下
//!   cargo run --example xinput -- drag X1 Y1 X2 Y2

use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    match args.first().map(String::as_str) {
        Some("move") => {
            motion(&conn, root, args[1].parse()?, args[2].parse()?)?;
        }
        Some("click") => {
            let button: u8 = args[1].parse()?;
            button_event(&conn, root, BUTTON_PRESS_EVENT, button)?;
            std::thread::sleep(Duration::from_millis(30));
            button_event(&conn, root, BUTTON_RELEASE_EVENT, button)?;
        }
        Some("scroll") => {
            let count: i64 = args[1].parse()?;
            let button = if count >= 0 { 4 } else { 5 };
            for _ in 0..count.abs() {
                button_event(&conn, root, BUTTON_PRESS_EVENT, button)?;
                std::thread::sleep(Duration::from_millis(20));
                button_event(&conn, root, BUTTON_RELEASE_EVENT, button)?;
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        Some("key") => {
            // key down KEYCODE / key up KEYCODE（如 Ctrl 通常是 37）
            let keycode: u8 = args[2].parse()?;
            let type_ = if args[1] == "down" {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            };
            conn.xtest_fake_input(type_, keycode, 0, root, 0, 0, 0)?;
        }
        Some("drag") => {
            let (x1, y1, x2, y2): (i16, i16, i16, i16) = (
                args[1].parse()?,
                args[2].parse()?,
                args[3].parse()?,
                args[4].parse()?,
            );
            motion(&conn, root, x1, y1)?;
            std::thread::sleep(Duration::from_millis(50));
            button_event(&conn, root, BUTTON_PRESS_EVENT, 1)?;
            // 分步移动，模拟真实拖动
            let steps = 12;
            for i in 1..=steps {
                let x = x1 + (x2 - x1) * i / steps;
                let y = y1 + (y2 - y1) * i / steps;
                motion(&conn, root, x, y)?;
                std::thread::sleep(Duration::from_millis(25));
            }
            std::thread::sleep(Duration::from_millis(50));
            button_event(&conn, root, BUTTON_RELEASE_EVENT, 1)?;
        }
        _ => {
            eprintln!(
                "用法: xinput move X Y | click N | scroll N | key down/up KEYCODE | drag X1 Y1 X2 Y2"
            );
            std::process::exit(2);
        }
    }
    conn.flush()?;
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn motion(conn: &RustConnection, root: u32, x: i16, y: i16) -> Result<(), Box<dyn std::error::Error>> {
    conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)?;
    conn.flush()?;
    Ok(())
}

fn button_event(
    conn: &RustConnection,
    root: u32,
    type_: u8,
    button: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.xtest_fake_input(type_, button, 0, root, 0, 0, 0)?;
    conn.flush()?;
    Ok(())
}
