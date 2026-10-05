//! 调试工具：列出 Wayland 合成器暴露的全局接口（检查协议支持）。
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

struct State;

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, _queue) = registry_queue_init::<State>(&conn)?;
    let mut names: Vec<String> = globals
        .contents()
        .clone_list()
        .iter()
        .map(|g| g.interface.clone())
        .collect();
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(())
}
