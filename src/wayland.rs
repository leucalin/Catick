//! Wayland 后端：wlr-layer-shell 的 overlay 层窗口（水印式悬浮）。
//!
//! 与 X11 后端的差异：
//! - 事件读取走 `prepare_read` + poll + `dispatch_pending` 三步；
//! - 位置用锚点（top|left）+ 边距（margin）表达；
//! - 支持 wp_viewporter + fractional-scale：按物理像素渲染，分数缩放屏幕下文字清晰。

use crate::overlay::{Input, Modifiers, Overlay, OverlayResult};
use rustix::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};
use wayland_client::backend::ReadEventsGuard;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_display, wl_output, wl_pointer, wl_region,
    wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use zwlr_layer_shell_v1::Layer;
use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};

/// Linux input-event-codes 的鼠标键值。
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

pub fn create() -> OverlayResult<Box<dyn Overlay>> {
    Ok(Box::new(WaylandOverlay::new()?))
}

/// 全部 Wayland 代理与事件翻译状态。
#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    fractional_mgr: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    output: Option<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    viewport: Option<wp_viewport::WpViewport>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,

    /// 已翻译、待应用层取走的事件
    pending: Vec<Input>,
    /// 已 attach、等待 compositor release 的缓冲
    pending_buffers: Vec<wl_buffer::WlBuffer>,
    configured: bool,
    scale: f64,
    /// 输出的物理像素尺寸（来自 mode 事件），用于默认位置
    output_phys: Option<(i32, i32)>,
    /// 指针的 surface 局部坐标
    pointer_pos: (f64, f64),
    /// relative-pointer（拖动用它拿到不受窗口移动影响的纯指针位移）
    relative_mgr: Option<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>,
    relative: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    /// 拖动状态：按键按下期间累计相对位移，作为虚拟根坐标
    button_down: bool,
    virtual_pos: (f64, f64),
    /// 自上次相对事件以来窗口被移动的量（测量/补偿用）
    applied_since_rel: (f64, f64),
    /// 位置记账（见 `PosTracker`）
    pos: PosTracker,
    sync_callback: Option<wl_callback::WlCallback>,
}

/// 拖动位置记账。
///
/// Wayland 的指针事件坐标是相对「合成器当前已应用」的窗口位置算出来的，
/// 而 set_margin 的提交是异步的。如果按本地已提交的位置补偿，事件积压时
/// 位移会被重复累加（表现为拖动飞出屏幕）。这里用 `wl_display.sync` 回调
/// 确认合成器真正处理到的位置，并用它换算虚拟根坐标。
#[derive(Debug, Default, Clone, Copy)]
struct PosTracker {
    /// 已提交给合成器的位置
    tracked: (i32, i32),
    /// 已确认被合成器处理的位置
    confirmed: (i32, i32),
    sync_in_flight: bool,
    sync_target: (i32, i32),
    needs_sync: bool,
}

impl PosTracker {
    /// 记录一次位置提交；返回需要发起 sync 时的新目标。
    fn submit(&mut self, pos: (i32, i32)) -> Option<(i32, i32)> {
        self.tracked = pos;
        if self.sync_in_flight {
            self.needs_sync = true;
            None
        } else {
            self.sync_in_flight = true;
            self.sync_target = pos;
            Some(pos)
        }
    }

    /// sync 回调返回；返回需要继续发起的下一个 sync 目标。
    fn sync_done(&mut self) -> Option<(i32, i32)> {
        self.sync_in_flight = false;
        self.confirmed = self.sync_target;
        if self.needs_sync {
            self.needs_sync = false;
            self.sync_in_flight = true;
            self.sync_target = self.tracked;
            Some(self.sync_target)
        } else {
            None
        }
    }

    /// surface 局部坐标 → 虚拟根坐标（应用层拖动算法与 X11 共用）。
    fn root(&self, local: (f64, f64)) -> (i32, i32) {
        (
            local.0.round() as i32 + self.confirmed.0,
            local.1.round() as i32 + self.confirmed.1,
        )
    }
}

pub struct WaylandOverlay {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    guard: Option<ReadEventsGuard>,
    fd: OwnedFd,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

impl WaylandOverlay {
    pub fn new() -> OverlayResult<Self> {
        let conn = Connection::connect_to_env()?;
        let fd = {
            let guard = conn
                .prepare_read()
                .ok_or("Wayland: connection not readable yet (unexpected during init)")?;
            let fd = rustix::io::dup(guard.connection_fd())?;
            drop(guard);
            fd
        };

        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let mut state = State {
            scale: 1.0,
            ..Default::default()
        };
        state.compositor = Some(globals.bind(&qh, 1..=6, ())?);
        state.shm = Some(globals.bind(&qh, 1..=1, ())?);
        state.layer_shell = Some(globals.bind(&qh, 1..=4, ())?);
        state.seat = globals.bind(&qh, 1..=7, ()).ok();
        state.viewporter = globals.bind(&qh, 1..=1, ()).ok();
        state.fractional_mgr = globals.bind(&qh, 1..=1, ()).ok();
        state.relative_mgr = globals.bind(&qh, 1..=1, ()).ok();
        let output_global = globals
            .contents()
            .clone_list()
            .into_iter()
            .find(|g| g.interface == "wl_output");
        if let Some(global) = output_global {
            state.output = Some(globals.registry().bind(
                global.name,
                global.version.min(4),
                &qh,
                (),
            ));
        }

        let surface = state
            .compositor
            .as_ref()
            .expect("compositor is bound")
            .create_surface(&qh, ());
        state.surface = Some(surface.clone());
        if let Some(vp) = &state.viewporter {
            state.viewport = Some(vp.get_viewport(&surface, &qh, ()));
        }
        if let Some(mgr) = &state.fractional_mgr {
            state.fractional = Some(mgr.get_fractional_scale(&surface, &qh, ()));
        }

        let layer_surface = state
            .layer_shell
            .as_ref()
            .expect("layer shell is bound")
            .get_layer_surface(
                &surface,
                state.output.as_ref(),
                Layer::Overlay,
                "catick".into(),
                &qh,
                (),
            );
        layer_surface.set_anchor(Anchor::Top | Anchor::Left);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_size(1, 1);
        layer_surface.set_margin(0, 0, 0, 0);
        state.layer_surface = Some(layer_surface);
        surface.commit();
        conn.flush()?;

        let mut overlay = WaylandOverlay {
            conn,
            queue,
            state,
            guard: None,
            fd,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        overlay.wait_configured(Duration::from_secs(5))?;
        if std::env::var_os("CATICK_DEBUG").is_some() {
            eprintln!(
                "catick[debug]: wayland caps seat={} pointer={} relative_mgr={} relative={} fractional={}",
                overlay.state.seat.is_some(),
                overlay.state.pointer.is_some(),
                overlay.state.relative_mgr.is_some(),
                overlay.state.relative.is_some(),
                overlay.state.fractional.is_some(),
            );
        }
        Ok(overlay)
    }

    /// 等待首个 configure（同时处理 seat/output 等初始化事件）。
    fn wait_configured(&mut self, timeout: Duration) -> OverlayResult<()> {
        let deadline = Instant::now() + timeout;
        while !self.state.configured {
            if Instant::now() > deadline {
                return Err("Wayland: timed out waiting for the layer surface configure".into());
            }
            let fd = self.poll_fd();
            // SAFETY: fd 由 self 持有，在 wait_configured 期间保持有效
            let fd_ref = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
            let mut fds = [rustix::event::PollFd::new(
                &fd_ref,
                rustix::event::PollFlags::IN,
            )];
            let ts = rustix::time::Timespec::try_from(Duration::from_millis(200))?;
            let _ = rustix::event::poll(&mut fds, Some(&ts));
            let readable = fds[0].revents().contains(rustix::event::PollFlags::IN);
            let _ = self.drain_events(readable);
        }
        Ok(())
    }

    /// 发起一次位置确认：compositor 处理完此前所有请求后会触发 done 回调。
    fn request_sync(&mut self, target: (i32, i32)) {
        let callback = self.conn.display().sync(&self.queue.handle(), ());
        self.state.sync_callback = Some(callback);
        self.state.pos.sync_target = target;
        self.state.pos.sync_in_flight = true;
    }
}

impl Overlay for WaylandOverlay {
    fn poll_fd(&mut self) -> RawFd {
        if self.guard.is_none() {
            self.guard = self.conn.prepare_read();
        }
        self.fd.as_raw_fd()
    }

    fn drain_events(&mut self, readable: bool) -> Vec<Input> {
        // guard 取出后无论是否可读都会 drop：不可读即取消这次 prepare_read
        if let Some(guard) = self.guard.take()
            && readable
            && let Err(err) = guard.read()
        {
            eprintln!("catick: failed to read Wayland events: {err}");
        }
        if let Err(err) = self.queue.dispatch_pending(&mut self.state) {
            eprintln!("catick: failed to dispatch Wayland events: {err}");
        }
        let _ = self.conn.flush();
        std::mem::take(&mut self.state.pending)
    }

    fn flush(&mut self) -> OverlayResult<()> {
        self.conn.flush()?;
        Ok(())
    }

    fn set_geometry(&mut self, x: i32, y: i32, w: u32, h: u32) -> OverlayResult<()> {
        let Some(layer_surface) = self.state.layer_surface.clone() else {
            return Ok(());
        };
        let size_changed = (w, h) != (self.w, self.h);
        let pos_changed = (x, y) != self.state.pos.tracked;
        if size_changed {
            layer_surface.set_size(w, h);
            if let Some(viewport) = &self.state.viewport {
                viewport.set_destination(w as i32, h as i32);
            }
        }
        if pos_changed {
            // 锚定左上角时：margin(top, right, bottom, left)
            layer_surface.set_margin(y, 0, 0, x);
        }
        if let Some(surface) = &self.state.surface {
            surface.commit();
        }
        self.state.applied_since_rel.0 += (x - self.x) as f64;
        self.state.applied_since_rel.1 += (y - self.y) as f64;
        self.x = x;
        self.y = y;
        self.w = w;
        self.h = h;
        if let Some(target) = self.state.pos.submit((x, y)) {
            self.request_sync(target);
        }
        self.conn.flush()?;
        Ok(())
    }

    fn geometry(&self) -> (i32, i32, u32, u32) {
        (self.x, self.y, self.w, self.h)
    }

    fn present(&mut self, buf: &[u8], w: u32, h: u32) -> OverlayResult<()> {
        let (rw, rh) = (w as i32, h as i32);
        if buf.len() < (rw * rh * 4) as usize {
            return Err(format!("frame buffer too small: {} < {rw}x{rh}x4", buf.len()).into());
        }
        let stride = rw * 4;

        // wl_shm 缓冲：memfd + 写入像素 + pool/buffer
        let file = rustix::fs::memfd_create("catick", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&file, buf.len() as u64)?;
        rustix::io::write(&file, buf)?;

        let shm = self.state.shm.clone().ok_or("Wayland: missing wl_shm")?;
        let qh = self.queue.handle();
        let pool = shm.create_pool(file.as_fd(), buf.len() as i32, &qh, ());
        let buffer = pool.create_buffer(0, rw, rh, stride, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        drop(file);

        let surface = self
            .state
            .surface
            .clone()
            .ok_or("Wayland: missing surface")?;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, rw, rh);
        if let Some(viewport) = &self.state.viewport {
            // 缓冲是物理像素，视口把它映射回逻辑尺寸（分数缩放的关键）
            viewport.set_destination(self.w as i32, self.h as i32);
        }
        surface.commit();
        self.state.pending_buffers.push(buffer);
        self.conn.flush()?;
        Ok(())
    }

    fn set_passthrough(&mut self, on: bool) -> OverlayResult<()> {
        let Some(surface) = &self.state.surface else {
            return Ok(());
        };
        if on {
            let compositor = self
                .state
                .compositor
                .clone()
                .ok_or("Wayland: missing compositor")?;
            let region = compositor.create_region(&self.queue.handle(), ());
            surface.set_input_region(Some(&region));
            region.destroy();
        } else {
            surface.set_input_region(None);
        }
        surface.commit();
        self.conn.flush()?;
        Ok(())
    }

    fn screen_size(&self) -> (u32, u32) {
        let scale = self.state.scale.max(0.1);
        match self.state.output_phys {
            Some((w, h)) => ((w as f64 / scale) as u32, (h as f64 / scale) as u32),
            None => ((1920.0 / scale) as u32, (1080.0 / scale) as u32),
        }
    }

    fn scale(&self) -> f64 {
        self.state.scale
    }

    fn name(&self) -> &'static str {
        "wayland"
    }
}

// ---- Dispatch：把 Wayland 事件翻译成 Overlay 的 Input ----

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

macro_rules! ignore_events {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

ignore_events!(wl_compositor::WlCompositor);
ignore_events!(wl_shm::WlShm);
ignore_events!(wl_shm_pool::WlShmPool);
ignore_events!(wl_surface::WlSurface);
ignore_events!(zwlr_layer_shell_v1::ZwlrLayerShellV1);
ignore_events!(wl_region::WlRegion);
ignore_events!(wp_viewporter::WpViewporter);
ignore_events!(wp_viewport::WpViewport);
ignore_events!(wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            // 释放后即可安全销毁；无 event 时缓冲区一直堆积是内存泄漏
            state.pending_buffers.retain(|b| b.id() != buffer.id());
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer_surface.ack_configure(serial);
                let _ = (width, height);
                // 尺寸由 set_geometry 决定；只有首个 configure 需要重绘。
                // 拖动时的 margin 提交也可能触发 configure，不能每次都重绘。
                if !state.configured {
                    state.configured = true;
                    state.pending.push(Input::Redraw);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.pending.push(Input::Close),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Scale { factor } => {
                if factor > 0 {
                    state.scale = factor as f64;
                    state.pending.push(Input::Redraw);
                }
            }
            wl_output::Event::Mode { width, height, .. } => {
                state.output_phys = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.scale = scale as f64 / 120.0;
            state.pending.push(Input::Redraw);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) {
                if state.pointer.is_none() {
                    let pointer = seat.get_pointer(qh, ());
                    // 拖动用：相对位移不受窗口自身移动影响
                    state.relative = state
                        .relative_mgr
                        .as_ref()
                        .map(|mgr| mgr.get_relative_pointer(&pointer, qh, ()));
                    state.pointer = Some(pointer);
                }
            } else {
                state.pointer = None;
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Wayland 不提供全局坐标：用「局部坐标 + 窗口位置」虚拟出根坐标，
        // 这样应用层的拖动算法与 X11 后端完全一致。
        let root = |state: &State| state.pos.root(state.pointer_pos);
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x, surface_y);
                // 相对指针拖动期间必须忽略绝对坐标：窗口自身移动会改变局部
                // 坐标，与相对位移叠加后会互相放大（拖动瞬间飞出屏幕）
                if state.button_down && state.relative.is_some() {
                    return;
                }
                let (x, y) = root(state);
                state.pending.push(Input::Motion {
                    root_x: x,
                    root_y: y,
                });
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } => {
                let code = match button {
                    BTN_LEFT => 1,
                    BTN_RIGHT => 3,
                    BTN_MIDDLE => 2,
                    _ => return,
                };
                // 按下时把虚拟指针归零到当前局部坐标；有 relative-pointer 时，
                // 整次拖动的根坐标都基于它，避免与「局部 + 确认位置」混用基准。
                let (x, y) = if state.relative.is_some() {
                    state.virtual_pos = state.pointer_pos;
                    (
                        state.virtual_pos.0.round() as i32,
                        state.virtual_pos.1.round() as i32,
                    )
                } else {
                    root(state)
                };
                // Wayland 指针事件不携带修饰键：Ctrl/Shift 组合请使用托盘菜单
                if button_state == wl_pointer::ButtonState::Pressed {
                    state.button_down = true;
                    state.pending.push(Input::ButtonPress {
                        button: code,
                        modifiers: Modifiers::default(),
                        root_x: x,
                        root_y: y,
                    });
                } else {
                    state.button_down = false;
                    state.pending.push(Input::ButtonRelease { button: code });
                }
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
                value,
                ..
            } => {
                // 正值为向下滚
                let button = if value > 0.0 { 5 } else { 4 };
                let (x, y) = root(state);
                state.pending.push(Input::ButtonPress {
                    button,
                    modifiers: Modifiers::default(),
                    root_x: x,
                    root_y: y,
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            // 合成器已处理到此处；销毁回调对象（标准做法）
            state.sync_callback = None;
            if let Some(target) = state.pos.sync_done() {
                let callback = conn.display().sync(qh, ());
                state.sync_callback = Some(callback);
                state.pos.sync_target = target;
                state.pos.sync_in_flight = true;
            }
        }
    }
}

ignore_events!(wl_display::WlDisplay);
ignore_events!(zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event {
            if std::env::var_os("CATICK_DEBUG").is_some() {
                eprintln!(
                    "catick[debug]: rel dx={dx:.1} dy={dy:.1} applied_since_rel=({:.1},{:.1})",
                    state.applied_since_rel.0, state.applied_since_rel.1
                );
            }
            state.applied_since_rel = (0.0, 0.0);
            // 只有按住按键（拖动中）才把位移转成拖动事件
            if state.button_down {
                state.virtual_pos.0 += dx;
                state.virtual_pos.1 += dy;
                state.pending.push(Input::Motion {
                    root_x: state.virtual_pos.0.round() as i32,
                    root_y: state.virtual_pos.1.round() as i32,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟「按下 → 拖动 → 合成器确认」的坐标换算，保证本地位移不会被重复累加
    /// （此前的实现会让拖动瞬间飞出屏幕）。
    #[test]
    fn drag_does_not_double_count_moves() {
        // 按下：局部 (100,100)，窗口在 (0,0)
        let mut pos = PosTracker::default();
        let press_root = pos.root((100.0, 100.0));
        let win_at_press = (0, 0);

        // 指针 +10，合成器尚未处理提交：局部坐标直接反映真实位移
        let root = pos.root((110.0, 100.0));
        assert_eq!(win_at_press.0 + (root.0 - press_root.0), 10);

        // 提交 + 合成器确认
        assert_eq!(pos.submit((10, 0)), Some((10, 0)));
        assert_eq!(pos.sync_done(), None);

        // 指针再 +5：局部坐标相对新窗口位置「回卷」，结果仍是真实位移
        let root = pos.root((105.0, 100.0));
        assert_eq!(win_at_press.0 + (root.0 - press_root.0), 15);

        // 事件积压：确认位置不变时，只有指针真实位移被计入（而非累加）
        let pos = PosTracker::default();
        let press = pos.root((100.0, 100.0));
        let mut x = 0;
        for local in [110.0, 120.0, 130.0] {
            let root = pos.root((local, 100.0));
            x = win_at_press.0 + (root.0 - press.0);
        }
        assert_eq!(x, 30);

        // 提交期间有未确认的位置时，sync 完成后补发确认
        let mut pos = PosTracker::default();
        assert!(pos.submit((10, 0)).is_some());
        assert!(pos.submit((20, 0)).is_none());
        assert_eq!(pos.sync_done(), Some((20, 0)));
        assert_eq!(pos.confirmed, (10, 0));
        assert_eq!(pos.sync_done(), None);
        assert_eq!(pos.confirmed, (20, 0));
    }
}
