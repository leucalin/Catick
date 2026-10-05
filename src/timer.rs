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
            PomodoroPhase::Work => "work",
            PomodoroPhase::ShortBreak => "short break",
            PomodoroPhase::LongBreak => "long break",
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
            long_break: parse_duration(&pomo.long_break).unwrap_or(Duration::from_secs(15 * 60)),
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

    /// 托盘进度环比例：倒计时/番茄钟为当前相位剩余占比，
    /// 秒表/时钟为一分钟内的进度。
    pub fn progress(&self, now: Duration) -> f64 {
        match self.mode {
            Mode::Clock => {
                use chrono::Timelike;
                chrono::Local::now().second() as f64 / 60.0
            }
            Mode::Stopwatch => (self.elapsed_at(now).as_secs_f64() % 60.0) / 60.0,
            Mode::Countdown => (self.remaining_at(now).as_secs_f64()
                / self.duration.as_secs_f64().max(1.0))
            .clamp(0.0, 1.0),
            Mode::Pomodoro => (self.remaining_at(now).as_secs_f64()
                / self.phase_duration(self.phase).as_secs_f64().max(1.0))
            .clamp(0.0, 1.0),
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
                let fmt = if self.clock_24h {
                    "%H:%M:%S"
                } else {
                    "%I:%M:%S"
                };
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
        // 刷新时刻必须用纳秒精度算：毫秒截断会把「不足 1ms 的小数部分」算成 0，
        // 误判为「正好落在边界上」而整睡 1 秒，越过边界后下一次又只睡 ~0ms，
        // 表现为秒数停两秒再跳一格。
        //
        // 两类显示的边界方向相反：
        // - 倒计时/番茄钟显示 ceil(剩余量)，剩余量的小数部分归零时变化 → 等小数部分本身；
        // - 时钟/秒表显示 floor(时间)，小数部分归零（进位）时变化 → 等 1s 减去小数部分。
        match self.mode {
            Mode::Clock => {
                let nanos = chrono::Local::now().timestamp_subsec_nanos();
                Some(nanos_until_carry(nanos))
            }
            Mode::Stopwatch => self
                .start
                .map(|_| nanos_until_carry(self.elapsed_at(now).subsec_nanos())),
            Mode::Countdown | Mode::Pomodoro => self
                .end
                .map(|_| nanos_until_truncate(self.remaining_at(now).subsec_nanos())),
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

/// 递减显示（ceil 剩余量）：小数部分归零即到边界，等小数部分本身；正好归零则等一整秒。
fn nanos_until_truncate(nanos: u32) -> Duration {
    if nanos == 0 {
        Duration::from_secs(1)
    } else {
        Duration::from_nanos(u64::from(nanos))
    }
}

/// 递增显示（floor 时间）：小数部分归零时要进位，等 1 秒减去小数部分。
fn nanos_until_carry(nanos: u32) -> Duration {
    Duration::from_nanos(u64::from(
        1_000_000_000u32.saturating_sub(nanos.min(999_999_999)),
    ))
}

fn format_hms(total: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn floor_secs(d: Duration) -> u64 {
    d.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// 小数部分不足 1ms 时，绝不能把「到下一个整秒」算成整整 1 秒
    /// （否则会停顿 2 秒再跳，表现为秒数卡一下）。
    #[test]
    fn countdown_next_update_handles_sub_millisecond_fractions() {
        let cfg = Config {
            mode: Mode::Countdown,
            duration: "10.0009s".into(),
            ..Config::default()
        };
        let t0 = Duration::from_secs(1_000);
        let timer = Timer::new(&cfg, t0);
        let next = timer.next_update(t0).expect("倒计时应给出下次刷新时刻");
        // 递减显示：小数部分 0.0009s 归零时变化，只等 0.9ms（而不是 1s，也不是 999.1ms）
        assert!(
            next >= Duration::from_micros(800) && next <= Duration::from_micros(1_000),
            "expected ~0.9ms, got {next:?}"
        );
    }

    /// 时钟/秒表是递增显示，边界在进位处，等的是「1 秒减去小数部分」。
    #[test]
    fn stopwatch_next_update_waits_for_carry() {
        let cfg = Config {
            mode: Mode::Stopwatch,
            ..Config::default()
        };
        let t0 = Duration::from_secs(1_000);
        let mut timer = Timer::new(&cfg, t0);
        timer.toggle(t0); // 启动秒表
        // 让秒表走到 elapsed 的小数部分 = 0.9ms 处
        let t = t0 + Duration::from_millis(2_000) + Duration::from_micros(900);
        let next = timer
            .next_update(t)
            .expect("运行中的秒表应给出下次刷新时刻");
        assert!(
            next >= Duration::from_micros(998_900) && next <= Duration::from_micros(999_200),
            "expected ~999.1ms, got {next:?}"
        );
    }

    /// 按 next_update 逐拍推进时，显示值必须每拍恰好减少 1 秒，绝不跳格。
    #[test]
    fn countdown_display_never_skips_a_second() {
        let cfg = Config {
            mode: Mode::Countdown,
            duration: "10.0009s".into(),
            ..Config::default()
        };
        let mut now = Duration::from_secs(5_000);
        let timer = Timer::new(&cfg, now);
        let secs = |text: &str| -> u64 {
            let mut parts = text.split(':').map(|p| p.parse::<u64>().unwrap());
            parts.next().unwrap() * 3600 + parts.next().unwrap() * 60 + parts.next().unwrap()
        };
        let mut previous = secs(&timer.display(now));
        for _ in 0..9 {
            now += timer.next_update(now).expect("仍在运行");
            let current = secs(&timer.display(now));
            assert_eq!(
                previous - current,
                1,
                "display jumped from {previous} to {current} at {now:?}"
            );
            previous = current;
        }
    }
}
