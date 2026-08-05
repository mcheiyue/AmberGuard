//! 息屏探测（小米/高通）：
//! - 成功读到 brightness>0 → On
//! - 成功读到全 0 → 再 dumpsys 兜底
//! - 读失败 → None（绝不当成 Off）
//! - 任一信号 On 即 On，避免「一直卡在息屏」

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    On,
    Off,
}

pub struct PowerStateManager {
    last: PowerState,
    /// 连续确认 Off 的次数（≥2 才切 Off）
    off_streak: u8,
    dump_cache: Option<(Instant, PowerState)>,
}

impl PowerStateManager {
    pub fn new() -> Self {
        Self {
            last: PowerState::On,
            off_streak: 0,
            dump_cache: None,
        }
    }

    pub fn current_state(&mut self) -> PowerState {
        let raw = detect_screen_raw(&mut self.dump_cache);
        let s = match raw {
            PowerState::On => {
                self.off_streak = 0;
                PowerState::On
            }
            PowerState::Off => {
                self.off_streak = self.off_streak.saturating_add(1);
                if self.off_streak >= 2 {
                    PowerState::Off
                } else {
                    self.last
                }
            }
        };
        if s != self.last {
            match s {
                PowerState::Off => log::info!("power_state: screen off — 降频采样、停主动 SCAN"),
                PowerState::On => log::info!("power_state: screen on — 恢复日用采样"),
            }
            self.last = s;
        }
        s
    }
}

fn detect_screen_raw(dump_cache: &mut Option<(Instant, PowerState)>) -> PowerState {
    let bl = read_panel_backlight();
    let ds = dumpsys_wakefulness(dump_cache);

    // 任一 On → On（防误灭）
    if bl == Some(PowerState::On) || ds == Some(PowerState::On) {
        return PowerState::On;
    }
    // 两者都明确 Off → Off
    if bl == Some(PowerState::Off) && ds == Some(PowerState::Off) {
        return PowerState::Off;
    }
    // 只有一侧 Off、另一侧未知：相信 dumpsys；都未知 → On
    if ds == Some(PowerState::Off) {
        return PowerState::Off;
    }
    if bl == Some(PowerState::Off) && ds.is_none() {
        // dumpsys 失败时，单靠亮度 0 仍可能是异常 → 偏 On 更安全？
        // 真息屏时 dumpsys 通常可用；不可用时宁可保持调度
        return PowerState::On;
    }
    PowerState::On
}

/// 成功读到 >0 → On；成功读到且全 0 → Off；读失败 → None
fn read_panel_backlight() -> Option<PowerState> {
    const CANDIDATES: &[&str] = &[
        "/sys/class/backlight/panel0-backlight/brightness",
        "/sys/devices/platform/soc/ae00000.qcom,mdss_mdp/backlight/panel0-backlight/brightness",
    ];
    // 注意：不读 actual_brightness（小米上常恒 0，会误导）
    let mut saw_zero = false;
    for p in CANDIDATES {
        match fs::read_to_string(p) {
            Ok(text) => {
                if let Ok(n) = text.trim().parse::<i64>() {
                    if n > 0 {
                        return Some(PowerState::On);
                    }
                    saw_zero = true;
                }
            }
            Err(_) => continue,
        }
    }
    if saw_zero {
        return Some(PowerState::Off);
    }
    // 泛扫 /sys/class/backlight
    if let Ok(entries) = fs::read_dir("/sys/class/backlight") {
        for e in entries.flatten() {
            let bright = e.path().join("brightness");
            if let Ok(text) = fs::read_to_string(&bright) {
                if let Ok(n) = text.trim().parse::<i64>() {
                    if n > 0 {
                        return Some(PowerState::On);
                    }
                    saw_zero = true;
                }
            }
        }
    }
    if saw_zero {
        Some(PowerState::Off)
    } else {
        None
    }
}

fn dumpsys_wakefulness(cache: &mut Option<(Instant, PowerState)>) -> Option<PowerState> {
    if let Some((t, st)) = *cache {
        if t.elapsed() < Duration::from_secs(2) {
            return Some(st);
        }
    }
    // 完整路径：daemon 经 setsid 启动时 PATH 可能不含 /system/bin
    let out = Command::new("/system/bin/dumpsys")
        .arg("power")
        .output()
        .or_else(|_| Command::new("dumpsys").arg("power").output())
        .ok()?;
    if out.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // 只认行首样式的赋值，避免枚举名/注释误伤
    let st = if line_has(&text, "mWakefulness=Awake") {
        PowerState::On
    } else if line_has(&text, "mWakefulness=Dozing")
        || line_has(&text, "mWakefulness=Asleep")
        || line_has(&text, "mWakefulness=Dreaming")
    {
        PowerState::Off
    } else if line_has(&text, "mScreenState=ON") {
        PowerState::On
    } else if line_has(&text, "mScreenState=OFF") {
        PowerState::Off
    } else {
        return None;
    };
    *cache = Some((Instant::now(), st));
    Some(st)
}

fn line_has(text: &str, pat: &str) -> bool {
    text.lines().any(|l| l.contains(pat))
}
