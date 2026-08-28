//! Android 通知模块
//! - event / ongoing 用 `cmd notification post`
//! - 必须以干净 shell(uid 2000) 身份发：MIUI/HyperOS 静默丢弃 root(uid 0) 或带 root 能力的通知
//! - 参考 GGAT_10007：su 2000 -c "cmd notification post"（清掉 root 能力）在 MIUI 上成功弹通知

use std::process::Command;
use std::time::Instant;

/// 通知渠道 ID（Android 8+ 要求；MIUI 无渠道会静默丢弃）
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

/// 以干净 shell(uid 2000) 身份运行 `cmd notification ...`。
/// 必须用 `su 2000 -c` 而非 setuid：su 会清掉 root 能力，MIUI/HyperOS 才放行；
/// 仅 setuid(2000) 会保留 root 能力，framework 仍静默丢弃该通知。
fn run_notify(args: &[&str]) {
    let mut cmd_str = String::from("cmd notification");
    for a in args {
        if a.contains(' ') || a.contains('"') || a.contains('\\') {
            cmd_str.push_str(&format!(" \"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")));
        } else {
            cmd_str.push(' ');
            cmd_str.push_str(a);
        }
    }
    let _ = Command::new("/system/bin/su")
        .args(["2000", "-c", &cmd_str])
        .output();
}

/// 递增的事件通知序号（保证 ID 唯一）
static mut EV_SEQ: u64 = 0;

/// 放行通知助手（MIUI/HyperOS 需要，否则部分 ROM 不显示 cmd 发的通知）
fn allow_assistant() {
    run_notify(&[
        "allow_assistant",
        "com.google.android.ext.services/android.ext.services.notification.Assistant",
    ]);
}

/// 发一次性事件通知（切换成功/失败、弱信号断开等）
pub fn event(title: &str, text: &str) {
    if !cmd_available() {
        return;
    }
    allow_assistant();
    let id = unsafe {
        EV_SEQ += 1;
        format!("{EVENT_PREFIX}{EV_SEQ}")
    };
    let msg = format!("{title}：{text}");
    run_notify(&[
        "post",
        "-S",
        "messaging",
        "--conversation",
        &id,
        "--message",
        &msg,
        "-t",
        title,
        &id,
        title,
    ]);
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
    allow_assistant();
    run_notify(&[
        "post",
        "--ongoing",
        "-t",
        "AmberGuard",
        "-channel",
        CHANNEL,
        ONGOING_ID,
        text,
    ]);
}

/// 清除常驻状态条（息屏/暂停/退出时）
pub fn cancel_ongoing() {
    if !cmd_available() {
        return;
    }
    run_notify(&["remove", ONGOING_ID]);
    unsafe { LAST_ONGOING = None; }
}

/// 测试通知（WebUI 测试按钮调用）
pub fn test() {
    event(
        "AmberGuard",
        "通知测试：如果你看到这条，说明通知功能正常工作。",
    );
}
