//! 息屏探测：读 backlight 亮度；失败当 On（不误降频）

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    On,
    Off,
}

pub struct PowerStateManager {
    last: PowerState,
}

impl PowerStateManager {
    pub fn new() -> Self {
        Self {
            last: PowerState::On,
        }
    }

    pub fn current_state(&mut self) -> PowerState {
        let s = detect_screen();
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

fn detect_screen() -> PowerState {
    // 常见 Android backlight 节点
    let roots = [
        "/sys/class/backlight",
        "/sys/devices/platform/soc",
    ];
    for root in roots {
        if let Some(st) = scan_backlight_dir(Path::new(root)) {
            return st;
        }
    }
    // fb blank: 0=unblank, 1/4=blank
    if let Ok(t) = fs::read_to_string("/sys/class/graphics/fb0/blank") {
        let v = t.trim();
        if v == "1" || v == "4" {
            return PowerState::Off;
        }
        if v == "0" {
            return PowerState::On;
        }
    }
    PowerState::On
}

fn scan_backlight_dir(dir: &Path) -> Option<PowerState> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        let bright = p.join("brightness");
        if !bright.is_file() {
            continue;
        }
        let text = fs::read_to_string(&bright).ok()?;
        let n: i64 = text.trim().parse().ok()?;
        // 有的机 max 很大，0 即灭
        return Some(if n <= 0 {
            PowerState::Off
        } else {
            PowerState::On
        });
    }
    None
}
