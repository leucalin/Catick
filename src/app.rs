//! 应用状态与事件循环：把后端事件、计时推进、渲染与配置持久化串起来。
//!
//! 循环约定（X11/Wayland 通用）：
//! 1. 先清空后端事件队列（非阻塞 drain）；
//! 2. 推进计时状态；
//! 3. 有变化则 cairo 渲染并贴帧；
//! 4. 用 poll 等待「后端 fd 可读」或「下一个到期时刻」，无事件无变化时零唤醒。

use crate::config::{Config, Mode, Position, TrayIconKind};
use crate::overlay::{Input, Modifiers, Overlay};
use crate::render::{self, Frame, Style};
use crate::timer::{self, Timer, TimerEvent};
use crate::tray::{self, Command, TrayChannels};
use rustix::event::{PollFd, PollFlags, poll};
use rustix::time::Timespec;
use std::error::Error;
use std::os::fd::BorrowedFd;
use std::process::Child;
use std::time::Duration;

/// 判定「点击」与「拖动」的位移阈值（像素）。
const DRAG_THRESHOLD: i32 = 4;
/// 字号调整步长与范围。
const FONT_STEP: f64 = 2.0;
const FONT_MIN: f64 = 8.0;
const FONT_MAX: f64 = 512.0;
/// 透明度调整步长与下限（0 会让窗口"消失"）。
const OPACITY_STEP: f64 = 0.05;
const OPACITY_MIN: f64 = 0.05;
/// poll 超时夹取，防止忙等与长时间不做维护。
const MIN_TIMEOUT: Duration = Duration::from_millis(2);
const MAX_TIMEOUT: Duration = Duration::from_secs(60);

pub struct App {
    cfg: Config,
    ov: Box<dyn Overlay>,
    style: Style,
    timer: Timer,
    frame: Option<Frame>,
    /// 上一帧对应的文本 / 编辑态 / 闪烁态，用于避免重复渲染
    frame_text: String,
    frame_edit: bool,
    frame_blink_on: bool,
    dirty: bool,
    edit_mode: bool,
    drag: Option<Drag>,
    /// 窗口的物理像素尺寸（渲染缓冲大小；逻辑尺寸 = 物理 / scale）
    phys_w: u32,
    phys_h: u32,
    /// 收到 Close（Wayland 下合成器关闭了 layer surface）后退出
    quit: bool,
    /// on_finish_cmd 的子进程，定期回收防僵尸
    children: Vec<Child>,
    /// 托盘字体菜单的可选项（启动时探测一次）
    fonts: Vec<String>,
    /// 图片托盘图标（含 GIF 动画帧）
    icon: Option<crate::icon::IconAnimation>,
    /// 图标动画起始时刻（用于按时间取帧）
    icon_started: Duration,
    /// 系统托盘（可能因缺少 SNI 宿主而未启动）
    tray: Option<TrayChannels>,
    /// 上次推送给托盘的显示键，用于避免无谓的 D-Bus 往返
    tray_key: String,
    /// 调试用：上次重绘时刻
    last_redraw: std::time::Instant,
    /// 进度环图标缓存：(量化键, 22px, 44px)
    icon_cache: Option<(String, Vec<u8>, Vec<u8>)>,
}

struct Drag {
    press_root: (i32, i32),
    win_pos: (i32, i32),
    moved: bool,
}

/// 调试日志：`CATICK_DEBUG=1` 时输出拖动等细节。
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var_os("CATICK_DEBUG").is_some() {
            eprintln!("catick[debug]: {}", format!($($arg)*));
        }
    };
}

/// 渲染托盘图标（22/44px）。
///
/// 进度环是**静态**的：图标一旦注册就不再改变。宿主（如 quickshell）在每次收到
/// NewIcon 时都会有一帧拿不到 pixmap，回落到字母占位符，表现为托盘闪一下——
/// 所以这里不能按进度重绘，精确时间由 tooltip 和悬浮窗给出。
/// 图片图标按动画帧取帧（GIF 会动，闪动是该模式的固有代价）。
fn render_tray_icons(
    cfg: &Config,
    timer: &crate::timer::Timer,
    style: &Style,
    icon: Option<&crate::icon::IconAnimation>,
    icon_started: Duration,
    now: Duration,
) -> (Vec<u8>, Vec<u8>, bool) {
    if cfg.tray_icon == TrayIconKind::Image
        && let Some(anim) = icon
    {
        let frame = anim.frame_at(icon_started);
        return (
            anim.render(frame, 22).unwrap_or_default(),
            anim.render(frame, 44).unwrap_or_default(),
            true,
        );
    }
    let _ = (timer, now); // 图标与时间无关（见上方说明）
    let glyph = crate::render::TrayGlyph { fraction: 1.0 };
    (
        crate::render::render_icon(glyph, 22, style.color).unwrap_or_default(),
        crate::render::render_icon(glyph, 44, style.color).unwrap_or_default(),
        false,
    )
}

/// 按配置加载图片托盘图标（失败只记日志，回退进度环）。
fn load_icon(cfg: &Config) -> Option<crate::icon::IconAnimation> {
    if cfg.tray_icon != TrayIconKind::Image {
        return None;
    }
    let path = cfg.tray_icon_path.as_deref()?;
    match crate::icon::load(std::path::Path::new(path)) {
        Ok(anim) => Some(anim),
        Err(err) => {
            eprintln!("catick: failed to load tray image {path}: {err}");
            None
        }
    }
}

/// 把窗口位置夹取到屏幕范围内（整个窗口可见）。
fn clamp_pos(screen: (u32, u32), pos: (i32, i32), size: (u32, u32)) -> (i32, i32) {
    let max_x = (screen.0 as i32 - size.0 as i32).max(0);
    let max_y = (screen.1 as i32 - size.1 as i32).max(0);
    (pos.0.clamp(0, max_x), pos.1.clamp(0, max_y))
}

/// 按缩放因子测量渲染尺寸（物理像素）。
fn measure_phys(style: &Style, scale: f64) -> (u32, u32) {
    let mut scaled = style.clone();
    scaled.size = (style.size * scale).max(1.0);
    render::measure(&scaled, render::TEMPLATE)
}

/// 物理像素 → 逻辑尺寸。
fn logical_size(phys_w: u32, phys_h: u32, scale: f64) -> (u32, u32) {
    let scale = scale.max(0.05);
    (
        ((phys_w as f64 / scale).ceil() as u32).max(1),
        ((phys_h as f64 / scale).ceil() as u32).max(1),
    )
}

/// 秒数转配置文件里的人类可读写法（3600 → "1h"）。
fn format_duration(secs: u64) -> String {
    if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

impl App {
    pub fn new(mut cfg: Config) -> Result<Self, Box<dyn Error>> {
        let mut ov = crate::overlay::create(&cfg)?;
        let style = Style::from_config(&cfg);
        let edit_mode = cfg.edit_on_start;

        // 物理像素渲染 + 逻辑坐标布局（Wayland 分数缩放；X11 下两者相同）
        let scale = ov.scale();
        let (phys_w, phys_h) = measure_phys(&style, scale);
        let (w, h) = logical_size(phys_w, phys_h, scale);
        let (screen_w, screen_h) = ov.screen_size();
        let (x, y) = cfg.position.map(|p| (p.x, p.y)).unwrap_or((
            ((screen_w.saturating_sub(w)) / 2) as i32,
            (screen_h / 5) as i32,
        ));
        // 夹取到屏幕内：配置里可能残留被拖飞时写下的坐标
        let (x, y) = clamp_pos((screen_w, screen_h), (x, y), (w, h));
        ov.set_geometry(x, y, w, h)?;
        // 编辑模式必须可交互，进入编辑前先关掉穿透
        ov.set_passthrough(cfg.click_through && !edit_mode)?;

        // 把最终生效的位置写回配置，编辑模式下才能基于它做居中缩放
        cfg.position = Some(Position { x, y });
        let now = timer::now();
        let timer = Timer::new(&cfg, now);

        // 自定义字体文件：注册进 fontconfig 后按家族名渲染
        let mut style = style;
        if let Some(path) = cfg.font_path.clone() {
            match crate::render::register_font_file(&path) {
                Some(family) => style.family = family,
                None => eprintln!(
                    "catick: failed to register font file {path}, falling back to {}",
                    style.family
                ),
            }
        }
        let fonts = crate::config::available_fonts(&style.family);
        let icon = load_icon(&cfg);
        let icon_started = timer::now();
        let tray = if cfg.tray {
            let icons = render_tray_icons(&cfg, &timer, &style, icon.as_ref(), icon_started, now);
            let state = tray::build_state(&cfg, &timer, &style, &fonts, icons);
            match tray::spawn(state) {
                Ok(channels) => Some(channels),
                Err(err) => {
                    eprintln!("catick: tray unavailable ({err}), continuing without it");
                    None
                }
            }
        } else {
            None
        };

        println!(
            "catick: backend={} scale={:.2} size={}x{}px @ ({x},{y}) mode={:?}",
            ov.name(),
            ov.scale(),
            phys_w,
            phys_h,
            cfg.mode,
        );

        Ok(App {
            cfg,
            ov,
            style,
            timer,
            frame: None,
            frame_text: String::new(),
            frame_edit: false,
            frame_blink_on: true,
            dirty: true,
            edit_mode,
            drag: None,
            fonts,
            icon,
            icon_started,
            phys_w,
            phys_h,
            quit: false,
            children: Vec::new(),
            tray,
            tray_key: String::new(),
            last_redraw: std::time::Instant::now(),
            icon_cache: None,
        })
    }

    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        self.redraw_if_needed()?;
        if self.timer.mode() == Mode::Pomodoro {
            println!(
                "catick: pomodoro · {} · running={}",
                self.timer.phase().label(),
                self.timer.is_running()
            );
        }
        // SAFETY: fd 由 ov 持有，在 ov 存活期间始终有效
        loop {
            let now = timer::now();
            let t_loop = std::time::Instant::now();
            self.sync_surface_size()?;

            let backend_raw = self.ov.poll_fd();
            // SAFETY: fd 由后端持有，在本轮迭代内保持有效
            let backend_fd = unsafe { BorrowedFd::borrow_raw(backend_raw) };
            let mut fds = vec![PollFd::new(&backend_fd, PollFlags::IN)];
            if let Some(tray) = &self.tray {
                fds.push(PollFd::new(&tray.wake_read, PollFlags::IN));
            }
            let mut timeout = self
                .timer
                .next_update(now)
                .map_or(MAX_TIMEOUT, |d| d.clamp(MIN_TIMEOUT, MAX_TIMEOUT));
            // 动画图标需要按帧唤醒
            if let Some(anim) = &self.icon
                && let Some(next) = anim.next_frame_in(self.icon_started)
            {
                timeout = timeout.min(next.clamp(MIN_TIMEOUT, MAX_TIMEOUT));
            }
            let work = t_loop.elapsed();
            if work.as_millis() > 50 {
                debug_log!("loop work before poll took {} ms", work.as_millis());
            }
            let t_poll = std::time::Instant::now();
            let ts = Timespec::try_from(timeout)?;
            match poll(&mut fds, Some(&ts)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(err) => return Err(err.into()),
            }
            let waited = t_poll.elapsed();
            let late = waited.saturating_sub(timeout);
            if late.as_millis() > 200 {
                debug_log!(
                    "poll woke {} ms late (waited {} ms, timeout {} ms)",
                    late.as_millis(),
                    waited.as_millis(),
                    timeout.as_millis()
                );
            }
            let backend_readable = fds[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP);

            // 时刻要用 poll 返回后的新值：旧的 now 可能已经过去了近一个轮询周期，
            // 用它处理滚轮调时会凭空多算一秒，推给托盘的状态也会晚一拍
            let now = timer::now();
            for ev in self.ov.drain_events(backend_readable) {
                self.handle_input(ev, now)?;
            }
            if self.quit {
                self.shutdown_tray();
                return Ok(());
            }
            if let Some(event) = self.timer.tick(now) {
                self.handle_timer_event(event);
            }
            self.redraw_if_needed()?;
            self.update_tray(now);
            self.ov.flush()?;
            self.reap_children();

            if self.handle_tray_commands(now)? {
                return Ok(());
            }
        }
    }

    fn shutdown_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.handle.shutdown().wait();
        }
    }

    /// 清空唤醒管道 + 执行托盘命令；返回是否退出。
    fn handle_tray_commands(&mut self, now: Duration) -> Result<bool, Box<dyn Error>> {
        let Some(tray) = &self.tray else {
            return Ok(false);
        };
        let mut buf = [0u8; 64];
        while rustix::io::read(&tray.wake_read, &mut buf).is_ok() {}
        let commands: Vec<Command> = tray.rx.try_iter().collect();
        for cmd in commands {
            debug_log!("tray command: {cmd:?}");
            if self.apply_command(cmd, now)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn apply_command(&mut self, cmd: Command, now: Duration) -> Result<bool, Box<dyn Error>> {
        let mut persist = false;
        match cmd {
            Command::TogglePause => {
                self.timer.toggle(now);
                self.dirty = true;
            }
            Command::Reset => {
                self.timer.reset();
                self.dirty = true;
            }
            Command::SetMode(mode) => {
                self.timer.set_mode(mode, now);
                self.cfg.mode = mode;
                self.dirty = true;
                persist = true;
            }
            Command::SetPreset(secs) => {
                self.timer.set_preset(now, secs);
                self.cfg.mode = Mode::Countdown;
                self.cfg.duration = format_duration(secs);
                self.dirty = true;
                persist = true;
            }
            Command::ToggleEditMode => self.toggle_edit_mode()?,
            Command::ToggleClickThrough => {
                self.cfg.click_through = !self.cfg.click_through;
                self.ov
                    .set_passthrough(self.cfg.click_through && !self.edit_mode)?;
                persist = true;
            }
            Command::ToggleClockFormat => {
                self.cfg.clock_24h = !self.cfg.clock_24h;
                self.timer.set_clock_24h(self.cfg.clock_24h);
                self.dirty = true;
                persist = true;
            }
            Command::FontSizeDelta(delta) => {
                self.style.size = (self.style.size + delta).clamp(FONT_MIN, FONT_MAX);
                self.cfg.font_size = self.style.size;
                self.dirty = true;
                persist = true;
            }
            Command::OpacityDelta(delta) => {
                let v = self.style.opacity + delta;
                self.style.opacity = (v.clamp(OPACITY_MIN, 1.0) * 100.0).round() / 100.0;
                self.cfg.opacity = self.style.opacity;
                self.dirty = true;
                persist = true;
            }
            Command::SetFont(font) => {
                self.style.family = font.clone();
                self.style.path = None;
                self.cfg.font_family = font;
                self.cfg.font_path = None;
                self.dirty = true;
                persist = true;
            }
            Command::SetColor(hex) => {
                if let Some(rgb) = crate::config::parse_color(&hex) {
                    self.style.color = rgb;
                    self.cfg.color = hex;
                    self.dirty = true;
                    persist = true;
                }
            }
            Command::BrowseFont => {
                self.spawn_pick(|| {
                    crate::picker::pick_file(
                        "Choose a font file",
                        "Fonts",
                        &["*.ttf", "*.otf", "*.ttc"],
                        "/usr/share/fonts",
                    )
                    .map(|path| Command::SetFontFile(path.to_string_lossy().into_owned()))
                });
            }
            Command::SetFontFile(path) => match crate::render::register_font_file(&path) {
                Some(family) => {
                    self.style.path = Some(path.clone());
                    self.style.family = family.clone();
                    self.cfg.font_path = Some(path);
                    self.cfg.font_family = family;
                    self.dirty = true;
                    persist = true;
                }
                None => eprintln!("catick: failed to register font file {path}"),
            },
            Command::SetTrayIcon(kind) => {
                self.cfg.tray_icon = kind;
                self.dirty = true;
                persist = true;
                if kind == TrayIconKind::Image && self.icon.is_none() {
                    // 选了图片但还没指定文件：直接弹选择框
                    return self.apply_command(Command::BrowseTrayIcon, now);
                }
            }
            Command::BrowseTrayIcon => {
                self.spawn_pick(|| {
                    crate::picker::pick_file(
                        "Choose a tray icon image",
                        "Images",
                        &["*.png", "*.gif", "*.jpg", "*.jpeg"],
                        "",
                    )
                    .map(|path| Command::SetTrayIconFile(path.to_string_lossy().into_owned()))
                });
            }
            Command::SetTrayIconFile(path) => {
                match crate::icon::load(std::path::Path::new(&path)) {
                    Ok(anim) => {
                        self.icon = Some(anim);
                        self.icon_started = now;
                        self.cfg.tray_icon = TrayIconKind::Image;
                        self.cfg.tray_icon_path = Some(path);
                        persist = true;
                    }
                    Err(err) => eprintln!("catick: failed to load tray image: {err}"),
                }
            }
            Command::SetLanguage(lang) => {
                self.cfg.language = lang;
                persist = true;
            }
            Command::OpenConfigDir => {
                // 用系统文件管理器打开配置目录（后台进程，不阻塞主循环）
                if let Some(dir) =
                    crate::config::Config::path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    && let Err(err) = std::process::Command::new("xdg-open").arg(&dir).spawn()
                {
                    eprintln!("catick: failed to open {}: {err}", dir.display());
                }
            }
            Command::Quit => {
                self.shutdown_tray();
                return Ok(true);
            }
        }
        if persist {
            self.persist();
        }
        Ok(false)
    }

    /// 显示文本 / 交互状态变化时才把新状态推给托盘。
    fn update_tray(&mut self, now: Duration) {
        // 先取图标（需要 &mut self 做缓存），最后再借用托盘句柄推送
        if self.tray.is_none() {
            return;
        }
        let anim_frame = self
            .icon
            .as_ref()
            .map(|anim| anim.frame_at(self.icon_started).to_string())
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{:?}|{}|{}|{:.1}|{:.2}|{:?}|{anim_frame}|{:?}|{}",
            self.timer.display(now),
            self.timer.is_running(),
            self.cfg.mode,
            self.edit_mode,
            self.cfg.click_through,
            self.style.size,
            self.style.opacity,
            (self.timer.mode() == Mode::Pomodoro).then(|| self.timer.phase()),
            self.cfg.language,
            self.cfg.tray_icon_path.as_deref().unwrap_or(""),
        );
        if key == self.tray_key {
            return;
        }
        self.tray_key = key;
        let icons = self.tray_icons(now);
        let mut state = tray::build_state(&self.cfg, &self.timer, &self.style, &self.fonts, icons);
        state.edit_mode = self.edit_mode;
        if let Some(tray) = &self.tray {
            tray.handle.update(|t| t.state = state);
        }
    }

    fn handle_input(&mut self, ev: Input, now: Duration) -> Result<(), Box<dyn Error>> {
        match ev {
            Input::ButtonPress {
                button,
                modifiers,
                root_x,
                root_y,
            } => match button {
                1 => {
                    let (wx, wy, _, _) = self.ov.geometry();
                    debug_log!("press root=({root_x},{root_y}) win=({wx},{wy})");
                    self.drag = Some(Drag {
                        press_root: (root_x, root_y),
                        win_pos: (wx, wy),
                        moved: false,
                    });
                }
                2 => {
                    // 中键 = 重置
                    self.timer.reset();
                    self.dirty = true;
                }
                3 => self.toggle_edit_mode()?,
                4 => self.on_wheel(modifiers, 1, now)?,
                5 => self.on_wheel(modifiers, -1, now)?,
                _ => {}
            },
            Input::ButtonRelease { button: 1, .. } => {
                if let Some(drag) = self.drag.take() {
                    if drag.moved {
                        let (x, y, _, _) = self.ov.geometry();
                        debug_log!("release moved -> ({x},{y})");
                        self.cfg.position = Some(Position { x, y });
                        self.persist();
                    } else {
                        // 单击 = 暂停 / 继续
                        self.timer.toggle(now);
                        self.dirty = true;
                    }
                }
            }
            Input::ButtonRelease { .. } => {}
            Input::Motion { root_x, root_y } => {
                if let Some(drag) = &self.drag {
                    let dx = root_x - drag.press_root.0;
                    let dy = root_y - drag.press_root.1;
                    let moved =
                        drag.moved || dx.abs() > DRAG_THRESHOLD || dy.abs() > DRAG_THRESHOLD;
                    let (_, _, w, h) = self.ov.geometry();
                    // 夹取位置：整个窗口保持在屏幕内，避免被拖丢
                    let (nx, ny) = clamp_pos(
                        self.ov.screen_size(),
                        (drag.win_pos.0 + dx, drag.win_pos.1 + dy),
                        (w, h),
                    );
                    debug_log!("motion root=({root_x},{root_y}) d=({dx},{dy}) -> ({nx},{ny})");
                    self.ov.set_geometry(nx, ny, w, h)?;
                    if let Some(drag) = self.drag.as_mut() {
                        drag.moved = moved;
                    }
                }
            }
            Input::Redraw => self.dirty = true,
            Input::Close => self.quit = true,
        }
        Ok(())
    }

    /// 渲染尺寸与当前测量结果不一致时重设窗口（字号/缩放变化后保持中心）。
    fn sync_surface_size(&mut self) -> Result<(), Box<dyn Error>> {
        let (pw, ph) = measure_phys(&self.style, self.ov.scale());
        if (pw, ph) == (self.phys_w, self.phys_h) {
            return Ok(());
        }
        let scale = self.ov.scale();
        let (x, y, lw, lh) = self.ov.geometry();
        let (cx, cy) = (x as f64 + lw as f64 / 2.0, y as f64 + lh as f64 / 2.0);
        let (nlw, nlh) = logical_size(pw, ph, scale);
        let nx = (cx - nlw as f64 / 2.0).round() as i32;
        let ny = (cy - nlh as f64 / 2.0).round() as i32;
        let (nx, ny) = clamp_pos(self.ov.screen_size(), (nx, ny), (nlw, nlh));
        self.ov.set_geometry(nx, ny, nlw, nlh)?;
        self.phys_w = pw;
        self.phys_h = ph;
        self.cfg.position = Some(Position { x: nx, y: ny });
        self.persist();
        self.dirty = true;
        Ok(())
    }

    /// 滚轮：普通模式调时间，编辑模式调字号（Ctrl 调透明度）。
    fn on_wheel(
        &mut self,
        modifiers: Modifiers,
        dir: i64,
        now: Duration,
    ) -> Result<(), Box<dyn Error>> {
        if self.edit_mode {
            if modifiers.ctrl {
                // 取整到 2 位小数，避免配置文件里出现 0.7999999999999998
                let v = self.style.opacity + OPACITY_STEP * dir as f64;
                self.style.opacity = (v.clamp(OPACITY_MIN, 1.0) * 100.0).round() / 100.0;
                self.cfg.opacity = self.style.opacity;
                self.dirty = true;
            } else {
                self.style.size =
                    (self.style.size + FONT_STEP * dir as f64).clamp(FONT_MIN, FONT_MAX);
                self.cfg.font_size = self.style.size;
                self.dirty = true;
            }
        } else {
            // 默认 ±1 分钟；Shift ±10s；Ctrl ±1h
            let delta = if modifiers.ctrl {
                3600
            } else if modifiers.shift {
                10
            } else {
                60
            };
            if self.timer.adjust(now, delta * dir) {
                self.dirty = true;
            }
        }
        Ok(())
    }

    /// 右键：进入 / 退出编辑模式；退出时落盘。
    fn toggle_edit_mode(&mut self) -> Result<(), Box<dyn Error>> {
        self.edit_mode = !self.edit_mode;
        self.ov
            .set_passthrough(self.cfg.click_through && !self.edit_mode)?;
        if !self.edit_mode {
            self.persist();
        }
        self.dirty = true;
        Ok(())
    }

    /// 生成托盘图标（22/44px）。
    ///
    /// 进度环按 1/60 圈量化并缓存：量化台阶没变时直接复用上一帧的字节，
    /// 这样大多数秒不会发出新图标——宿主替换图标那一帧会短暂回落到占位符
    /// （表现为托盘闪一下）。图片图标按动画帧更新，不参与该缓存。
    fn tray_icons(&mut self, now: Duration) -> (Vec<u8>, Vec<u8>, bool) {
        if self.cfg.tray_icon == TrayIconKind::Image && self.icon.is_some() {
            // 动画图标本来就要逐帧更新，不参与缓存
            return render_tray_icons(
                &self.cfg,
                &self.timer,
                &self.style,
                self.icon.as_ref(),
                self.icon_started,
                now,
            );
        }
        let key = format!(
            "{:.3},{:.3},{:.3}",
            self.style.color.0, self.style.color.1, self.style.color.2
        );
        if let Some((cached_key, small, big)) = &self.icon_cache
            && *cached_key == key
        {
            return (small.clone(), big.clone(), false);
        }
        let (small, big, _) = render_tray_icons(
            &self.cfg,
            &self.timer,
            &self.style,
            None,
            self.icon_started,
            now,
        );
        self.icon_cache = Some((key, small.clone(), big.clone()));
        (small, big, false)
    }

    /// 在后台线程调用文件选择框（对话框会阻塞），结果经命令通道送回主循环。
    fn spawn_pick<F>(&self, pick: F)
    where
        F: FnOnce() -> Option<Command> + Send + 'static,
    {
        let Some(tray) = &self.tray else {
            eprintln!("catick: tray disabled, cannot open a file dialog");
            return;
        };
        let tx = tray.tx.clone();
        let Ok(wake) = tray.wake_write.try_clone() else {
            return;
        };
        std::thread::spawn(move || {
            if let Some(cmd) = pick() {
                let _ = tx.send(cmd);
                let _ = rustix::io::write(&wake, b"c");
            }
        });
    }

    /// 只把界面上调整过的字段写回磁盘配置，
    /// 避免把 `--time` / `--font` 这类一次性 CLI 覆盖固化进配置文件。
    fn persist(&mut self) {
        let mut disk = Config::load();
        disk.position = self.cfg.position;
        disk.font_size = self.cfg.font_size;
        disk.opacity = self.cfg.opacity;
        disk.mode = self.cfg.mode;
        disk.duration = self.cfg.duration.clone();
        disk.click_through = self.cfg.click_through;
        disk.clock_24h = self.cfg.clock_24h;
        disk.font_family = self.cfg.font_family.clone();
        disk.font_path = self.cfg.font_path.clone();
        disk.color = self.cfg.color.clone();
        disk.tray_icon = self.cfg.tray_icon;
        disk.tray_icon_path = self.cfg.tray_icon_path.clone();
        disk.language = self.cfg.language;
        disk.save();
    }

    fn handle_timer_event(&mut self, event: TimerEvent) {
        self.dirty = true;
        match event {
            TimerEvent::Finished | TimerEvent::PhaseAdvanced(_) => self.run_finish_cmd(),
        }
    }

    fn run_finish_cmd(&mut self) {
        let Some(cmd) = self.cfg.on_finish_cmd.clone() else {
            return;
        };
        match std::process::Command::new("sh").arg("-c").arg(&cmd).spawn() {
            Ok(child) => self.children.push(child),
            Err(err) => eprintln!("catick: failed to run on_finish_cmd: {err}"),
        }
    }

    fn reap_children(&mut self) {
        self.children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
    }

    /// 有变化（文本/尺寸/编辑态/闪烁）才重新渲染并贴帧。
    fn redraw_if_needed(&mut self) -> Result<(), Box<dyn Error>> {
        let before = self.frame_text.clone();
        let now = timer::now();
        let text = self.timer.display(now);
        // 归零后 500ms 相位闪烁：弱相位渲染为全透明帧
        let blink_on = !self.timer.is_finished() || timer::blink_on(now);
        if !self.dirty
            && text == self.frame_text
            && self.edit_mode == self.frame_edit
            && blink_on == self.frame_blink_on
        {
            return Ok(());
        }

        let mut style = self.style.clone();
        style.size = (style.size * self.ov.scale()).max(1.0);
        if !blink_on {
            style.opacity = 0.0;
        }
        let frame = render::render(&style, &text, self.phys_w, self.phys_h, self.edit_mode)?;
        self.ov.present(&frame.pixels, frame.width, frame.height)?;

        if std::env::var_os("CATICK_DEBUG").is_some() {
            let now_i = std::time::Instant::now();
            let gap = now_i.duration_since(self.last_redraw);
            if gap.as_millis() > 1200 {
                debug_log!(
                    "redraw {before:?} -> {text:?} after a {} ms gap",
                    gap.as_millis()
                );
            }
            self.last_redraw = now_i;
        }
        self.frame = Some(frame);
        self.frame_text = text;
        self.frame_edit = self.edit_mode;
        self.frame_blink_on = blink_on;
        self.dirty = false;
        Ok(())
    }
}
