//! 计时状态机：时钟 / 倒计时 / 秒表 / 番茄钟。
//!
//! 时间记账用 `CLOCK_BOOTTIME`（系统休眠期间照常流逝，符合"厨房定时器"语义，
//! 也不受 NTP 校时影响）。倒计时类状态用「截止时刻 + 剩余量」双表示：
//! 运行时以截止时刻为准，暂停时折算回剩余量。

use crate::config::{Config, Mode, parse_duration};
use rustix::time::{ClockId, clock_gettime};
use std::time::Duration;

/// 单调、包含休眠时间的当前时刻。
pub fn now() -> Duration {
    let ts = clock_gettime(ClockId::Boottime);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// 时钟每 500ms 翻转一次闪烁相位。
pub fn blink_on(now: Duration) -> bool {
    (now.as_millis() / 500).is_multiple_of(2)
}

/// 倒计时上限 99:59:59。
const MAX_COUNTDOWN: Duration = Duration::from_secs(99 * 3600 + 59 * 60 + 59);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PomodoroPhase {
    Work,
    ShortBreak,
    LongBreak,
}

impl PomodoroPhase {
    /// 托盘提示文本（步骤 6 接入托盘）。
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            PomodoroPhase::Work => "工作",
            PomodoroPhase::ShortBreak => "短休",
            PomodoroPhase::LongBreak => "长休",
        }
    }
}

/// 计时器广播给应用层的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerEvent {
    /// 倒计时归零
    Finished,
    /// 番茄钟自动进入下一阶段
    PhaseAdvanced(PomodoroPhase),
}

#[derive(Debug, Clone)]
pub struct Timer {
    pub mode: Mode,
    // 倒计时 / 番茄钟共享
    remaining: Duration,
    end: Option<Duration>,
    finished: bool,
    // 秒表
    elapsed: Duration,
    start: Option<Duration>,
    // 番茄钟
    phase: PomodoroPhase,
    round: u32,
    // 配置快照
    duration: Duration,
    clock_24h: bool,
    work: Duration,
    short_break: Duration,
    long_break: Duration,
    cycles: u32,
}

impl Timer {
    pub fn new(cfg: &Config, now: Duration) -> Self {
        let pomo = &cfg.pomodoro;
        let work = parse_duration(&pomo.work).unwrap_or(Duration::from_secs(25 * 60));
        let mut timer = Timer {
            mode: cfg.mode,
            remaining: cfg.duration(),
            end: None,
            finished: false,
            elapsed: Duration::ZERO,
            start: None,
            phase: PomodoroPhase::Work,
            round: 1,
            duration: cfg.duration(),
            clock_24h: cfg.clock_24h,
            work,
            short_break: parse_duration(&pomo.short_break).unwrap_or(Duration::from_secs(5 * 60)),
            long_break: parse_duration(&pomo.long_break)
                .unwrap_or(Duration::from_secs(15 * 60)),
            cycles: pomo.cycles.max(1),
        };
        match cfg.mode {
            // 倒计时/番茄钟启动即运行（对齐 Catime 首启行为）
            Mode::Countdown => timer.end = Some(now + timer.remaining),
            Mode::Pomodoro => {
                timer.remaining = timer.work;
                timer.end = Some(now + timer.work);
            }
            Mode::Clock | Mode::Stopwatch => {}
        }
        timer
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn phase(&self) -> PomodoroPhase {
        self.phase
    }

    /// 切换模式（各模式状态独立保留，托盘菜单使用，步骤 6 接入）。
    #[allow(dead_code)]
    pub fn set_mode(&mut self, mode: Mode, now: Duration) {
        self.mode = mode;
        if mode == Mode::Pomodoro && self.end.is_none() && !self.finished {
            self.remaining = self.phase_duration(self.phase);
            self.end = Some(now + self.remaining);
        }
    }

    /// 切换 12/24 小时制（时钟模式）。
    pub fn set_clock_24h(&mut self, on: bool) {
        self.clock_24h = on;
    }

    /// 快捷预设：设定倒计时时长并立即从该时长开始运行。
    pub fn set_preset(&mut self, now: Duration, secs: u64) {
        self.mode = Mode::Countdown;
        self.duration = Duration::from_secs(secs.max(1));
        self.remaining = self.duration;
        self.finished = false;
        self.end = Some(now + self.duration);
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn is_running(&self) -> bool {
        match self.mode {
            Mode::Clock => true,
            Mode::Stopwatch => self.start.is_some(),
            Mode::Countdown | Mode::Pomodoro => self.end.is_some(),
        }
    }

    /// 单击：暂停 / 继续（倒计时归零后单击则重新开始）。
    pub fn toggle(&mut self, now: Duration) {
        match self.mode {
            Mode::Clock => {}
            Mode::Stopwatch => match self.start {
                Some(start) => {
                    self.elapsed += now.saturating_sub(start);
                    self.start = None;
                }
                None => self.start = Some(now),
            },
            Mode::Countdown | Mode::Pomodoro => {
                if self.finished {
                    self.finished = false;
                    self.remaining = self.duration_for_start();
                    self.end = Some(now + self.remaining);
                } else if let Some(end) = self.end {
                    self.remaining = end.saturating_sub(now);
                    self.end = None;
                } else {
                    self.remaining = self.remaining.max(Duration::from_secs(1));
                    self.end = Some(now + self.remaining);
                }
            }
        }
    }

    /// 重置：倒计时回到配置时长、番茄钟回到第一轮工作、秒表清零（均暂停）。
    pub fn reset(&mut self) {
        match self.mode {
            Mode::Clock => {}
            Mode::Stopwatch => {
                self.elapsed = Duration::ZERO;
                self.start = None;
            }
            Mode::Countdown => {
                self.finished = false;
                self.remaining = self.duration;
                self.end = None;
            }
            Mode::Pomodoro => {
                self.phase = PomodoroPhase::Work;
                self.round = 1;
                self.remaining = self.work;
                self.end = None;
            }
        }
    }

    /// 滚轮调整时间（秒，可负）；仅倒计时/番茄钟有效。
    pub fn adjust(&mut self, now: Duration, delta_secs: i64) -> bool {
        if !matches!(self.mode, Mode::Countdown | Mode::Pomodoro) {
            return false;
        }
        let current = self.remaining_at(now);
        let target = if delta_secs >= 0 {
            (current + Duration::from_secs(delta_secs as u64)).min(MAX_COUNTDOWN)
        } else {
            current.saturating_sub(Duration::from_secs(delta_secs.unsigned_abs()))
        };
        self.finished = false;
        if self.end.is_some() {
            self.end = Some(now + target);
        } else {
            self.remaining = target;
        }
        true
    }

    /// 推进状态；到点则返回事件。
    pub fn tick(&mut self, now: Duration) -> Option<TimerEvent> {
        let end = self.end?;
        if now < end {
            return None;
        }
        match self.mode {
            Mode::Countdown => {
                self.finished = true;
                self.end = None;
                self.remaining = Duration::ZERO;
                Some(TimerEvent::Finished)
            }
            Mode::Pomodoro => {
                match self.phase {
                    PomodoroPhase::Work => {
                        self.phase = if self.round.is_multiple_of(self.cycles) {
                            PomodoroPhase::LongBreak
                        } else {
                            PomodoroPhase::ShortBreak
                        };
                    }
                    PomodoroPhase::ShortBreak => {
                        self.phase = PomodoroPhase::Work;
                        self.round += 1;
                    }
                    PomodoroPhase::LongBreak => {
                        self.phase = PomodoroPhase::Work;
                        self.round = 1;
                    }
                }
                self.remaining = self.phase_duration(self.phase);
                self.end = Some(now + self.remaining);
                Some(TimerEvent::PhaseAdvanced(self.phase))
            }
            Mode::Clock | Mode::Stopwatch => None,
        }
    }

    /// 显示文本（等宽 `HH:MM:SS`；12 小时制同样按三位数对齐）。
    pub fn display(&self, now: Duration) -> String {
        match self.mode {
            Mode::Clock => {
                let fmt = if self.clock_24h { "%H:%M:%S" } else { "%I:%M:%S" };
                chrono::Local::now().format(fmt).to_string()
            }
            Mode::Stopwatch => format_hms(floor_secs(self.elapsed_at(now))),
            Mode::Countdown | Mode::Pomodoro => format_hms(ceil_secs(self.remaining_at(now))),
        }
    }

    /// 距离下一次需要重绘（秒跳变/闪烁翻转）还有多久；None 表示无需定时唤醒。
    pub fn next_update(&self, now: Duration) -> Option<Duration> {
        if self.finished {
            return Some(Duration::from_millis(500 - (now.as_millis() % 500) as u64));
        }
        match self.mode {
            Mode::Clock => {
                let ms = chrono::Local::now().timestamp_subsec_millis() as u64;
                Some(Duration::from_millis(1000 - ms.min(999)))
            }
            Mode::Stopwatch => self
                .start
                .map(|_| Duration::from_millis(1000 - self.elapsed_at(now).subsec_millis() as u64)),
            Mode::Countdown | Mode::Pomodoro => self.end.map(|_| {
                // 显示按秒向上取整：剩余量跨越整秒时刷新
                let ms = self.remaining_at(now).subsec_millis() as u64;
                Duration::from_millis(if ms == 0 { 1000 } else { ms })
            }),
        }
    }

    fn duration_for_start(&self) -> Duration {
        match self.mode {
            Mode::Pomodoro => self.phase_duration(self.phase),
            _ => self.duration,
        }
    }

    fn phase_duration(&self, phase: PomodoroPhase) -> Duration {
        match phase {
            PomodoroPhase::Work => self.work,
            PomodoroPhase::ShortBreak => self.short_break,
            PomodoroPhase::LongBreak => self.long_break,
        }
    }

    fn remaining_at(&self, now: Duration) -> Duration {
        match self.end {
            Some(end) => end.saturating_sub(now),
            None => self.remaining,
        }
    }

    fn elapsed_at(&self, now: Duration) -> Duration {
        match self.start {
            Some(start) => self.elapsed + now.saturating_sub(start),
            None => self.elapsed,
        }
    }
}

fn format_hms(total: u64) -> String {
    format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn floor_secs(d: Duration) -> u64 {
    d.as_secs()
}
