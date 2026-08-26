//! Android 通知模块
//! - event: `cmd notification post` 一次性通知（切换/弱信号/错误）
//! - ongoing: 常驻状态条，周期更新（亮屏时）
//! - 检测 `cmd` 可用性，不可用则静默跳过

use std::process::Command;
use std::time::Instant;

/// 通知渠道 ID（Android 自动建渠）
const CHANNEL: &str = "amberguard";
/// 常驻通知 ID（固定，重复 post 即更新）
const ONGOING_ID: &str = "amber_ongoing";
/// 事件通知 ID 前缀（每次唯一）
const EVENT_PREFIX: &str = "amber_ev_";

/// `cmd` 是否可用（首次检测后缓存）
static mut CMD_OK: Option<bool> = None;

fn cmd_available() -> bool {
    unsafe {
        if let Some(ok) = CMD_OK {
            return ok;
        }
        let ok = Command::new("/system/bin/cmd")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        CMD_OK = Some(ok);
        ok
    }
}

/// 递增的事件通知序号（保证 ID 唯一）
static mut EV_SEQ: u64 = 0;

/// 发一次性事件通知（切换成功/失败、弱信号断开等）
pub fn event(title: &str, text: &str) {
    if !cmd_available() {
        return;
    }
    let id = unsafe {
        EV_SEQ += 1;
        format!("{EVENT_PREFIX}{EV_SEQ}")
    };
    let out = Command::new("/system/bin/cmd")
        .args(["notification", "post", "-t", title, &id, text])
        .output();
    if let Err(e) = out {
        log::debug!("notify event failed: {e}");
    }
}

/// 上次 ongoing 更新时间 + 文本（防抖）
static mut LAST_ONGOING: Option<(Instant, String)> = None;

/// 更新常驻状态条（方案 C）。
/// 传入完整状态文本。若文本与上次相同且未超间隔，则跳过。
pub fn ongoing(text: &str, min_interval_secs: u64) {
    if !cmd_available() || min_interval_secs == 0 {
        return;
    }
    let now = Instant::now();
    unsafe {
        if let Some((t, prev)) = &LAST_ONGOING {
            if prev == text && now.duration_since(*t).as_secs() < min_interval_secs {
                return;
            }
        }
        LAST_ONGOING = Some((now, text.to_string()));
    }
    let out = Command::new("/system/bin/cmd")
        .args([
            "notification", "post", "--ongoing", "-t", "AmberGuard",
            ONGOING_ID, text,
        ])
        .output();
    if let Err(e) = out {
        log::debug!("notify ongoing failed: {e}");
    }
}

/// 清除常驻状态条（息屏/暂停/退出时）
pub fn cancel_ongoing() {
    if !cmd_available() {
        return;
    }
    let _ = Command::new("/system/bin/cmd")
        .args(["notification", "remove", ONGOING_ID])
        .output();
    unsafe { LAST_ONGOING = None; }
}

/// 测试通知（WebUI 测试按钮调用）
pub fn test() {
    event("AmberGuard", "通知测试：如果你看到这条，说明通知功能正常工作。");
}
