//! 息屏探测（小米/高通）：
//! 1) panel backlight brightness（快）
//! 2) 连续 2 次 Off 才确认灭屏（防抖）
//! 3) brightness==0 时用 dumpsys power mWakefulness 兜底（AOD/异常 0 亮度）
//! 失败偏 On，避免「一直卡在息屏」误降频

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
    /// 连续读到 Off 的次数（≥2 才切到 Off）
    off_streak: u8,
    /// dumpsys 缓存，避免每秒 fork
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
                // 需连续 2 次 Off 才确认（约 2 个主循环 tick）
                if self.off_streak >= 2 {
                    PowerState::Off
                } else {
                    // 保持上一稳定态，避免单次 0 亮度误灭
                    self.last
                }
            }
        };
        if s != self.last {
            match s {
                PowerState::Off => self.on_screen_off(),
                PowerState::On => self.on_screen_on(),
            }
            self.last = s;
        }
        s
    }

    pub fn on_screen_off(&mut self) {
        log::info!("power_state: screen off — 降频采样、停主动 SCAN");
    }

    pub fn on_screen_on(&mut self) {
        log::info!("power_state: screen on — 恢复日用采样");
    }
}

fn detect_screen_raw(dump_cache: &mut Option<(Instant, PowerState)>) -> PowerState {
    // 1) 明确的 panel backlight（小米 panel0-backlight）
    if let Some(st) = read_panel_backlight() {
        match st {
            PowerState::On => return PowerState::On,
            PowerState::Off => {
                // brightness=0：可能真息屏，也可能驱动异常 → dumpsys 兜底
                if let Some(d) = dumpsys_wakefulness(dump_cache) {
                    return d;
                }
                return PowerState::Off;
            }
        }
    }
    // 2) 泛扫 backlight 目录
    if let Some(st) = scan_backlight_dir(Path::new("/sys/class/backlight")) {
        if st == PowerState::On {
            return PowerState::On;
        }
        if let Some(d) = dumpsys_wakefulness(dump_cache) {
            return d;
        }
        return st;
    }
    // 3) 仅 dumpsys
    if let Some(d) = dumpsys_wakefulness(dump_cache) {
        return d;
    }
    // 失败当 On，避免误锁息屏
    PowerState::On
}

fn read_panel_backlight() -> Option<PowerState> {
    const CANDIDATES: &[&str] = &[
        "/sys/class/backlight/panel0-backlight/brightness",
        "/sys/class/backlight/panel0-backlight/actual_brightness",
        "/sys/devices/platform/soc/ae00000.qcom,mdss_mdp/backlight/panel0-backlight/brightness",
    ];
    for p in CANDIDATES {
        if let Ok(text) = fs::read_to_string(p) {
            if let Ok(n) = text.trim().parse::<i64>() {
                // 注意：本机 actual_brightness 常恒 0，brightness 才准；两个都试，任一 >0 即 On
                if n > 0 {
                    return Some(PowerState::On);
                }
                // 读到 0 继续试下一个节点；全 0 再返回 Off
            }
        }
    }
    // 若 panel 节点存在且都是 0
    if Path::new("/sys/class/backlight/panel0-backlight/brightness").is_file() {
        return Some(PowerState::Off);
    }
    None
}

fn scan_backlight_dir(dir: &Path) -> Option<PowerState> {
    let entries = fs::read_dir(dir).ok()?;
    let mut saw = false;
    let mut any_on = false;
    for e in entries.flatten() {
        let bright = e.path().join("brightness");
        if !bright.is_file() {
            continue;
        }
        saw = true;
        if let Ok(text) = fs::read_to_string(&bright) {
            if let Ok(n) = text.trim().parse::<i64>() {
                if n > 0 {
                    any_on = true;
                }
            }
        }
    }
    if !saw {
        return None;
    }
    Some(if any_on {
        PowerState::On
    } else {
        PowerState::Off
    })
}

/// 解析 `dumpsys power`：mWakefulness=Awake|Dozing|Asleep|Dreaming
fn dumpsys_wakefulness(cache: &mut Option<(Instant, PowerState)>) -> Option<PowerState> {
    if let Some((t, st)) = *cache {
        if t.elapsed() < Duration::from_secs(3) {
            return Some(st);
        }
    }
    let out = Command::new("dumpsys").arg("power").output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let st = if text.contains("mWakefulness=Awake") {
        PowerState::On
    } else if text.contains("mWakefulness=Dozing")
        || text.contains("mWakefulness=Asleep")
        || text.contains("mWakefulness=Dreaming")
    {
        // Dozing=息屏显示，按息屏降频处理
        PowerState::Off
    } else if text.contains("mScreenState=ON") {
        PowerState::On
    } else if text.contains("mScreenState=OFF") {
        PowerState::Off
    } else {
        return None;
    };
    *cache = Some((Instant::now(), st));
    Some(st)
}
