use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::band_bond::{
    best_on_band, dual_band_pair_saved, home_contains, link_in_home, merge_scan_aps,
    network_id_for_ssid, parse_cmd_scan_results, parse_list_networks, parse_scan_results,
    scan_views_filtered, stems_from_ssids,
};
use crate::config::{Config, ConfigPatch};
use crate::health_score::health_score;
use crate::power_state::{PowerState, PowerStateManager};
use crate::state_machine::{StateMachine, SwitchHint};
use crate::station_info::{iw_station_dump, parse_iw_station, retry_rate, StationSample};
use crate::web::{Readiness, ReadyStep, StatusSnapshot, SwitchEvent, ThresholdsView};
use crate::wpa_ctrl::WpaCtrl;

const HISTORY_CAP: usize = 20;
const HISTORY_PATH: &str = "/data/adb/amberguard/history.json";

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_history() -> VecDeque<SwitchEvent> {
    let mut q = VecDeque::with_capacity(HISTORY_CAP);
    let Ok(raw) = std::fs::read_to_string(HISTORY_PATH) else {
        return q;
    };
    let list = web::events_from_json(&raw);
    for ev in list.into_iter().take(HISTORY_CAP) {
        q.push_back(ev);
    }
    q
}

fn save_history(hist: &VecDeque<SwitchEvent>) {
    let list: Vec<SwitchEvent> = hist.iter().cloned().collect();
    let s = web::events_to_json(&list);
    let _ = std::fs::create_dir_all("/data/adb/amberguard");
    let _ = std::fs::write(HISTORY_PATH, s);
}

/// 运行模式中文
fn mode_zh(mode: &str) -> &'static str {
    match mode {
        "eco" => "省电",
        "pause" => "暂停",
        _ => "日用",
    }
}

/// 守护状态中文（与面板 labelSm 对齐）
fn power_zh(ps: &str) -> String {
    let p = ps.trim();
    if p.starts_with("USER_HOLD") {
        return "手切保护中".into();
    }
    if p.starts_with("SOFT_PAUSE") {
        return "观影保护中".into();
    }
    match p {
        "Idle" | "idle" => "守护中".into(),
        "GradientDetect" => "观察中".into(),
        "Switching" => "切换中".into(),
        "Penalty" => "冷却中".into(),
        "Frozen" => "冻结".into(),
        "PAUSE" | "Pause" => "已暂停".into(),
        "OUT_OF_HOME" => "非家网".into(),
        "WEAK_OFF" => "弱信号已断".into(),
        "SCREEN_OFF" => "息屏降频".into(),
        "" => "—".into(),
        other => other.to_string(),
    }
}

/// 保护种类：手切 vs 观影（不再混称）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldKind {
    Manual,
    SoftPause,
}

impl HoldKind {
    fn as_str(self) -> &'static str {
        match self {
            HoldKind::Manual => "manual",
            HoldKind::SoftPause => "soft_pause",
        }
    }
    fn block_zh(self, rem: u64) -> String {
        match self {
            HoldKind::Manual => format!("手切保护中（剩 {rem}s，不自动切网）"),
            HoldKind::SoftPause => format!("观影保护中（剩 {rem}s，不自动切网）"),
        }
    }
    fn summary_zh(self, rem: u64) -> String {
        match self {
            HoldKind::Manual => format!("手切保护 · {rem}s（可点结束保护）"),
            HoldKind::SoftPause => format!("观影保护 · {rem}s（可点结束保护）"),
        }
    }
    fn power_state(self, rem: u64) -> String {
        match self {
            HoldKind::Manual => format!("USER_HOLD({rem}s)"),
            HoldKind::SoftPause => format!("SOFT_PAUSE({rem}s)"),
        }
    }
}

struct HoldState {
    until: Instant,
    kind: HoldKind,
}

/// status.txt：状态边沿立即写；相同内容最多 15s 写一次
fn write_status_txt(line: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex as StdMutex;
    static LAST_TS: AtomicU64 = AtomicU64::new(0);
    static LAST_LINE: StdMutex<String> = StdMutex::new(String::new());
    let now = unix_now();
    let changed = {
        let mut prev = LAST_LINE.lock().unwrap_or_else(|e| e.into_inner());
        if *prev != line {
            *prev = line.to_string();
            true
        } else {
            false
        }
    };
    let prev_ts = LAST_TS.load(Ordering::Relaxed);
    if !changed && now.saturating_sub(prev_ts) < 15 {
        return;
    }
    LAST_TS.store(now, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("/data/adb/amberguard");
    let _ = std::fs::write("/data/adb/amberguard/status.txt", format!("{line}\n"));
}

fn band_zh(band: &str) -> &'static str {
    match band {
        "5" => "5G",
        "2.4" => "2.4G",
        _ => "未知频段",
    }
}

/// 状态页「阈值对照」人话：调三个阈值后应能直接读到差异
fn threshold_hint_zh(
    on_preferred: bool,
    score: f32,
    rssi: i32,
    switch_th: f32,
    detect_th: f32,
    up_rssi: i32,
    best_pref: Option<i32>,
) -> String {
    if on_preferred {
        if score < switch_th {
            format!(
                "偏好下切带：分 {score:.0} < 下切线 {switch_th:.0}（{rssi} dBm）→ 防抖后切后备频段"
            )
        } else if score < detect_th {
            format!(
                "偏好观察带：下切线 {switch_th:.0} ≤ 分 {score:.0} < 观察线 {detect_th:.0} → 加勤扫描"
            )
        } else {
            format!(
                "偏好稳定带：分 {score:.0} ≥ 观察线 {detect_th:.0}（{rssi} dBm）→ 守护中"
            )
        }
    } else {
        match best_pref {
            Some(b) if b >= up_rssi => format!(
                "后备频段·上切就绪：偏好侧最强 {b} dBm ≥ 上切线 {up_rssi} → 防抖后回偏好"
            ),
            Some(b) => format!(
                "后备频段·等待：偏好侧最强 {b} dBm < 上切线 {up_rssi}"
            ),
            None => format!(
                "后备频段·寻找偏好 AP（需 ≥ {up_rssi} dBm）"
            ),
        }
    }
}

/// 面具列表 / status.txt 用的中文一行（模块实时信息）
fn status_line_zh(
    mode: &str,
    ssid: &str,
    band: &str,
    score: f32,
    power_state: &str,
    block_reason: &str,
) -> String {
    let ssid_show = if ssid.is_empty() { "未连接" } else { ssid };
    let st = if !block_reason.is_empty() {
        block_reason.to_string()
    } else {
        power_zh(power_state)
    };
    format!(
        "AmberGuard · {} · {} · {} · 分{:.0} · {}",
        mode_zh(mode),
        ssid_show,
        band_zh(band),
        score,
        st
    )
}

/// 把一行状态写入模块 module.prop 的 description=（面具列表展示）
fn update_module_description(line: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = unix_now();
    let prev = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(prev) < 15 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);

    let prop = module_prop_path();
    let Some(prop) = prop else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&prop) else {
        return;
    };
    // 单行、去掉 description 非法字符
    let desc: String = line
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '#' | '=' => ' ',
            c => c,
        })
        .take(96)
        .collect::<String>()
        .trim()
        .to_string();
    if desc.is_empty() {
        return;
    }
    let mut out = String::with_capacity(raw.len() + 32);
    let mut replaced = false;
    for l in raw.lines() {
        if l.starts_with("description=") {
            out.push_str("description=");
            out.push_str(&desc);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str("description=");
        out.push_str(&desc);
        out.push('\n');
    }
    let _ = std::fs::write(prop, out);
}

fn module_prop_path() -> Option<std::path::PathBuf> {
    // /data/adb/modules/AmberGuard/bin/amberguard → ../module.prop
    let exe = std::fs::read_link("/proc/self/exe").ok()?;
    let mod_dir = exe.parent()?.parent()?;
    let p = mod_dir.join("module.prop");
    if p.is_file() {
        Some(p)
    } else {
        let fallback = std::path::PathBuf::from("/data/adb/modules/AmberGuard/module.prop");
        if fallback.is_file() {
            Some(fallback)
        } else {
            None
        }
    }
}

/// 模块目录路径
fn module_dir() -> Option<std::path::PathBuf> {
    let exe = std::fs::read_link("/proc/self/exe").ok()?;
    exe.parent()?.parent().map(|p| p.to_path_buf())
}

/// 记录启动时关键文件的 mtime（用于热更新检测）
struct ModuleFileMtimes {
    binary: Option<std::time::SystemTime>,
    sepolicy: Option<std::time::SystemTime>,
    post_fs_data: Option<std::time::SystemTime>,
    service_sh: Option<std::time::SystemTime>,
}

fn record_module_mtimes() -> ModuleFileMtimes {
    let dir = module_dir().unwrap_or_else(|| std::path::PathBuf::from("/data/adb/modules/AmberGuard"));
    let mtime = |name: &str| std::fs::metadata(dir.join(name))
        .ok()
        .and_then(|m| m.modified().ok());
    ModuleFileMtimes {
        binary: std::fs::metadata("/proc/self/exe").ok().and_then(|m| m.modified().ok()),
        sepolicy: mtime("sepolicy.rule"),
        post_fs_data: mtime("post-fs-data.sh"),
        service_sh: mtime("service.sh"),
    }
}

/// 检查 bin/amberguard 的 mtime 是否变化（模块被更新）
fn binary_changed(start: &ModuleFileMtimes) -> bool {
    let cur = std::fs::metadata("/proc/self/exe")
        .ok()
        .and_then(|m| m.modified().ok());
    match (start.binary, cur) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// 检查需重启的文件是否变化
fn reboot_files_changed(start: &ModuleFileMtimes) -> bool {
    let dir = module_dir().unwrap_or_else(|| std::path::PathBuf::from("/data/adb/modules/AmberGuard"));
    let mtime = |name: &str| std::fs::metadata(dir.join(name))
        .ok()
        .and_then(|m| m.modified().ok());
    let sep = mtime("sepolicy.rule");
    let pfs = mtime("post-fs-data.sh");
    let svc = mtime("service.sh");
    sep != start.sepolicy || pfs != start.post_fs_data || svc != start.service_sh
}

/// 从 module.prop 读取版本号和 configResetNeeded 标记
fn read_module_prop() -> (String, bool) {
    let path = module_prop_path().unwrap_or_else(|| std::path::PathBuf::from("/data/adb/modules/AmberGuard/module.prop"));
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut version = String::new();
    let mut reset_needed = false;
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("version=") {
            version = v.trim().to_string();
        }
        if line.contains("configResetNeeded=true") || line.contains("configResetNeeded=1") {
            reset_needed = true;
        }
    }
    (version, reset_needed)
}

fn push_history(hist: &Arc<Mutex<VecDeque<SwitchEvent>>>, ev: SwitchEvent) {
    if let Ok(mut h) = hist.lock() {
        h.push_front(ev);
        while h.len() > HISTORY_CAP {
            h.pop_back();
        }
        save_history(&h);
    }
}

/// 切后是否真到了目标。
/// 异名双频：SSID 一致即可（家网常有多 BSSID，钉死 BSSID 会误判失败并狂重试→系统停用网络）。
/// 同 SSID 漫游：有目标 BSSID 时再比 BSSID。
fn link_reached_peer(
    st: &crate::wpa_ctrl::WpaStatus,
    peer_ssid: &str,
    peer_bssid: &str,
    require_bssid: bool,
) -> bool {
    if st.wpa_state != "COMPLETED" {
        return false;
    }
    let got_ssid = st.ssid.as_deref().unwrap_or("");
    if got_ssid.is_empty() || got_ssid != peer_ssid {
        return false;
    }
    if !require_bssid {
        return true;
    }
    let want_b = peer_bssid.trim();
    if want_b.is_empty() {
        return true;
    }
    st.bssid
        .as_deref()
        .map(|b| b.eq_ignore_ascii_case(want_b))
        .unwrap_or(false)
}

/// L3 分类结果（刀6）：ok / portal / timeout / unreachable / skip
#[derive(Debug, Clone, PartialEq, Eq)]
enum L3Kind {
    Ok,
    Portal,
    Timeout,
    Unreachable,
    Skip,
}

impl L3Kind {
    fn as_prefix(&self) -> &'static str {
        match self {
            L3Kind::Ok => "ok",
            L3Kind::Portal => "portal",
            L3Kind::Timeout => "timeout",
            L3Kind::Unreachable => "net",
            L3Kind::Skip => "skip",
        }
    }
    fn hist_result(&self) -> &'static str {
        match self {
            L3Kind::Ok => "Ok",
            L3Kind::Portal => "OkL3Portal",
            L3Kind::Timeout => "OkL3Timeout",
            L3Kind::Unreachable => "OkL3Net",
            L3Kind::Skip => "Ok",
        }
    }
}

/// L3：国内优先。gstatic/msft 常 DNS 失败只作次选，不因此判切网失败。
/// 204=通；HTTP 其它码像门户；全连不上=timeout/net。
fn l3_probe(timeout: Duration) -> Result<L3Kind, (L3Kind, String)> {
    // 主终点：小米 ROM 连通性（国内稳）
    let primary: &[(&str, u16, &str, &str)] = &[(
        "connect.rom.miui.com",
        80,
        "/generate_204",
        "connect.rom.miui.com",
    )];
    // 次选：国外/微软（失败不刷屏，仅记 last）
    let secondary: &[(&str, u16, &str, &str)] = &[
        (
            "connectivitycheck.gstatic.com",
            80,
            "/generate_204",
            "connectivitycheck.gstatic.com",
        ),
        (
            "www.msftconnecttest.com",
            80,
            "/connecttest.txt",
            "www.msftconnecttest.com",
        ),
    ];
    let mut last = (L3Kind::Timeout, String::from("无终点"));
    let mut saw_portal = false;
    let mut portal_detail = String::new();
    for (host, port, path, hdr) in primary.iter().chain(secondary.iter()) {
        match l3_http_one(host, *port, path, hdr, timeout) {
            Ok(L3Kind::Ok) => return Ok(L3Kind::Ok),
            Ok(L3Kind::Portal) => {
                saw_portal = true;
                portal_detail = format!("{host} 非 204");
            }
            Ok(k) => last = (k, host.to_string()),
            Err((k, e)) => {
                // DNS 失败只 debug，避免日志刷 OkL3Warn 噪声
                if e.contains("DNS") {
                    log::debug!("L3 skip {host}: {e}");
                }
                last = (k, e);
            }
        }
    }
    if saw_portal {
        return Err((L3Kind::Portal, portal_detail));
    }
    // 裸 IP：国内 DNS 优先
    for ip in &["223.5.5.5:80", "223.5.5.5:53", "1.1.1.1:80"] {
        if let Ok(addr) = ip.parse() {
            if TcpStream::connect_timeout(&addr, timeout).is_ok() {
                return Ok(L3Kind::Ok);
            }
        }
    }
    Err(last)
}

fn l3_http_one(
    host: &str,
    port: u16,
    path: &str,
    host_hdr: &str,
    timeout: Duration,
) -> Result<L3Kind, (L3Kind, String)> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| (L3Kind::Unreachable, format!("DNS {host}: {e}")))?
        .next()
        .ok_or_else(|| (L3Kind::Unreachable, format!("DNS {host}: 无结果")))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| {
        let msg = e.to_string();
        let kind = if msg.contains("timed") || msg.contains("10060") || msg.contains("110") {
            L3Kind::Timeout
        } else {
            L3Kind::Unreachable
        };
        (kind, format!("connect {host}: {e}"))
    })?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!("GET {path} HTTP/1.0\r\nHost: {host_hdr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| (L3Kind::Timeout, format!("write: {e}")))?;
    let mut buf = [0u8; 128];
    let n = stream
        .read(&mut buf)
        .map_err(|e| (L3Kind::Timeout, format!("read: {e}")))?;
    if n == 0 {
        return Err((L3Kind::Timeout, "空响应".into()));
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let line = head.lines().next().unwrap_or("");
    // generate_204：仅 204 算干净；200/30x 多像门户劫持
    if line.contains(" 204") {
        Ok(L3Kind::Ok)
    } else if path.contains("connecttest") && line.contains(" 200") {
        Ok(L3Kind::Ok)
    } else if line.contains(" 200") || line.contains(" 30") || line.contains(" 302") || line.contains(" 301") {
        Ok(L3Kind::Portal)
    } else if line.contains("HTTP/") {
        Err((L3Kind::Portal, format!("状态行: {line}")))
    } else {
        Err((L3Kind::Timeout, format!("非 HTTP: {line}")))
    }
}

/// 状态页一句人话（刀4）
fn status_summary_zh(
    band: &str,
    score: f32,
    mode: &str,
    hold_rem: u64,
    pen_rem: u64,
    in_home: bool,
    home_n: usize,
    screen_off: bool,
    paused: bool,
    on_preferred: bool,
    lock_rem: u64,
    prefer_5: bool,
) -> String {
    if paused {
        return "已暂停 · 仅观测".into();
    }
    if hold_rem > 0 {
        // hold_kind 由调用方写入 summary 前已区分；此处兜底
        return format!("保护中 · {hold_rem}s（可点结束保护）");
    }
    if screen_off {
        return "息屏降频 · 不主动切".into();
    }
    if pen_rem > 0 {
        return format!("切换冷却中 · {pen_rem}s");
    }
    if lock_rem > 0 {
        return format!("短时锁 AP · {lock_rem}s（防踢回）");
    }
    if home_n > 0 && !in_home {
        return "非家网 · 仅观测".into();
    }
    let band_zh = if band == "5" { "5G" } else if band == "2.4" { "2.4G" } else { "未知频段" };
    let pref = if prefer_5 { "5G" } else { "2.4G" };
    let mode_s = if mode == "eco" { "省电" } else { "日用" };
    if on_preferred {
        if score >= 70.0 {
            format!("{band_zh} 良好 · {mode_s}守护中")
        } else if score >= 30.0 {
            format!("{band_zh} 观察中 · 分 {score:.0}")
        } else {
            format!("{band_zh} 偏弱 · 可能下切")
        }
    } else {
        format!("{band_zh} 后备 · 等{pref}回暖")
    }
}

mod band_bond;
mod config;
mod file_log;
mod health_score;
mod notify;
mod power_state;
mod scanner;
mod state_machine;
mod station_info;
mod web;
mod wifi_framework;
mod wpa_ctrl;

fn json_resp(body: String, code: StatusCode) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(body).with_status_code(code);
    for kv in &[
        (&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]),
        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
    ] {
        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
            r.add_header(h);
        }
    }
    r
}

fn read_body(req: &mut tiny_http::Request) -> String {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    buf
}

fn main() {
    // 重启后模块文件已生效，清除上一轮热更新提示
    let _ = std::fs::remove_file("/data/adb/amberguard/update.txt");
    let config = Arc::new(Mutex::new(Config::load().expect("config load")));
    {
        let c = config.lock().unwrap();
        file_log::init(&c.log_level);
        log::info!("AmberGuard Phase 4 daemon started");
        log::info!("log file: {}", file_log::log_path_display());
        log::info!(
            "Interface: {}, Listen: {}, mode={}, detect={}, switch={}, up_rssi={}, log={}",
            c.interface,
            c.listen,
            c.mode,
            c.score_detect_threshold,
            c.score_switch_threshold,
            c.upswitch_rssi_min_dbm,
            c.log_level
        );
    }

    let wpa = match WpaCtrl::auto_connect() {
        Ok(w) => {
            log::info!("wpa_supplicant connected");
            w
        }
        Err(e) => {
            log::error!("wpa_supplicant connect failed: {e} — offline mode");
            offline_loop();
        }
    };
    let wpa = Arc::new(Mutex::new(wpa));
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));
    // 手切 / 观影 分种类保护；HTTP 可 clear / soft-pause
    let hold_state: Arc<Mutex<Option<HoldState>>> = Arc::new(Mutex::new(None));
    let history: Arc<Mutex<VecDeque<SwitchEvent>>> =
        Arc::new(Mutex::new(load_history()));
    let mut sm = StateMachine::new();
    let mut power = PowerStateManager::new();

    let listen = config.lock().unwrap().listen.clone();
    let addr: SocketAddr = listen.parse().expect("bad listen address");
    let server = Server::http(addr).expect("listen");
    log::info!("HTTP server listening on {addr}");

    let snapshot_http = Arc::clone(&snapshot);
    let config_http = Arc::clone(&config);
    let hold_http = Arc::clone(&hold_state);
    let wpa_http = Arc::clone(&wpa);
    let history_http = Arc::clone(&history);
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let method = req.method().clone();
            // 去掉 query
            let path = url.split('?').next().unwrap_or(&url);

            let resp = match (method, path) {
                (Method::Get, "/api/status") => {
                    let s = snapshot_http.lock().unwrap();
                    let json = s.to_json();
                    json_resp(json, StatusCode(200))
                }
                (Method::Get, "/api/history") => {
                    let h = history_http.lock().unwrap();
                    let list: Vec<&SwitchEvent> = h.iter().collect();
                    let evs = web::events_to_json(
                        &list.iter().map(|e| (*e).clone()).collect::<Vec<_>>()
                    );
                    let body = format!(r#"{{"ok":true,"events":{evs}}}"#);
                    json_resp(body, StatusCode(200))
                }
                (Method::Post, "/api/history/clear") | (Method::Get, "/api/history/clear") => {
                    if let Ok(mut h) = history_http.lock() {
                        h.clear();
                        save_history(&h);
                    }
                    json_resp(r#"{"ok":true}"#.into(), StatusCode(200))
                }
                (Method::Get, "/api/readiness") => {
                    let c = config_http.lock().unwrap();
                    let persisted = Config::is_persisted();
                    let home_n = c.home_aps.len();
                    let saved = {
                        let raw = wpa_http
                            .lock()
                            .ok()
                            .and_then(|w| w.list_networks().ok())
                            .unwrap_or_default();
                        let wpa_only: Vec<String> = parse_list_networks(&raw)
                            .into_iter()
                            .map(|(_, s)| s)
                            .filter(|s| !s.is_empty())
                            .collect();
                        // 小米 wpa LIST 常只有当前网；合并 cmd/WifiConfigStore
                        wifi_framework::merge_saved_ssids(&wpa_only)
                    };
                    let (dual_ok, dual_hint) = dual_band_pair_saved(&saved);
                    let snap = snapshot_http.lock().unwrap();
                    let steps = vec![
                        ReadyStep {
                            id: "persist".into(),
                            ok: persisted,
                            title: "保存配置".into(),
                            hint: if persisted {
                                "config.toml 已存在".into()
                            } else {
                                "点下方「初始化」写入默认并落盘".into()
                            },
                        },
                        ReadyStep {
                            id: "system_dual".into(),
                            ok: dual_ok,
                            title: "系统已保存双频网络".into(),
                            hint: dual_hint,
                        },
                        ReadyStep {
                            id: "home".into(),
                            ok: home_n > 0,
                            title: "勾选家网 AP（建议）".into(),
                            hint: if home_n > 0 {
                                format!("已选 {home_n} 个")
                            } else {
                                "可选；多路由强烈建议。空=不限制".into()
                            },
                        },
                    ];
                    let body = Readiness {
                        persisted,
                        home_configured: home_n > 0,
                        home_ap_count: home_n,
                        saved_ssids: saved,
                        steps,
                        block_reason: snap.block_reason.clone(),
                    };
                    json_resp(body.to_json(), StatusCode(200))
                }
                (Method::Get, "/api/config") => {
                    let c = config_http.lock().unwrap();
                    let body = serde_json::to_string(&Config::api_response(&c))
                        .unwrap_or_else(|_| "{}".into());
                    json_resp(body, StatusCode(200))
                }
                (Method::Post, "/api/config") | (Method::Put, "/api/config") => {
                    let body = read_body(&mut req);
                    match serde_json::from_str::<ConfigPatch>(&body) {
                        Ok(patch) => {
                            let mut c = config_http.lock().unwrap();
                            match c.apply_patch(patch) {
                                Ok(()) => match c.save() {
                                    Ok(()) => {
                                        file_log::set_level(&c.log_level);
                                        log::info!(
                                            "config updated: detect={} switch={} up_rssi={} mode={} log={}",
                                            c.score_detect_threshold,
                                            c.score_switch_threshold,
                                            c.upswitch_rssi_min_dbm,
                                            c.mode,
                                            c.log_level
                                        );
                                        // 同步 mode 到 snapshot
                                        if let Ok(mut s) = snapshot_http.lock() {
                                            if c.mode == "pause" {
                                                s.power_state = "PAUSE".into();
                                            } else if s.power_state == "PAUSE" {
                                                s.power_state = "ON".into();
                                            }
                                            s.thresholds = ThresholdsView {
                                                score_detect_threshold: c.score_detect_threshold,
                                                score_switch_threshold: c.score_switch_threshold,
                                                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                                mode: c.mode.clone(),
                                            };
                                            s.last_error.clear();
                                        }
                                        let body = serde_json::to_string(&Config::api_response(&c))
                                            .unwrap_or_else(|_| "{}".into());
                                        json_resp(body, StatusCode(200))
                                    }
                                    Err(e) => json_resp(
                                        format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                                        StatusCode(500),
                                    ),
                                },
                                Err(e) => json_resp(
                                    format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                                    StatusCode(400),
                                ),
                            }
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"JSON: {e}\"}}"),
                            StatusCode(400),
                        ),
                    }
                }
                (Method::Post, "/api/config/preset/daily")
                | (Method::Post, "/api/config/preset/stable")
                | (Method::Post, "/api/config/preset/sensitive")
                | (Method::Get, "/api/config/preset/daily")
                | (Method::Get, "/api/config/preset/stable")
                | (Method::Get, "/api/config/preset/sensitive") => {
                    let id = path.rsplit('/').next().unwrap_or("daily");
                    let mut c = config_http.lock().unwrap();
                    match c.apply_preset(id).and_then(|_| c.save()) {
                        Ok(()) => {
                            if let Ok(mut s) = snapshot_http.lock() {
                                s.thresholds = ThresholdsView {
                                    score_detect_threshold: c.score_detect_threshold,
                                    score_switch_threshold: c.score_switch_threshold,
                                    upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                    mode: c.mode.clone(),
                                };
                            }
                            let body = serde_json::to_string(&Config::api_response(&c))
                                .unwrap_or_else(|_| "{}".into());
                            json_resp(body, StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(400),
                        ),
                    }
                }
                (Method::Get, "/api/mode/pause") | (Method::Post, "/api/mode/pause") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "pause".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "PAUSE".into();
                        s.thresholds.mode = "pause".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"pause\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/mode/daily") | (Method::Post, "/api/mode/daily") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "daily".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "ON".into();
                        s.thresholds.mode = "daily".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"daily\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/mode/eco") | (Method::Post, "/api/mode/eco") => {
                    let mut c = config_http.lock().unwrap();
                    c.mode = "eco".into();
                    let _ = c.save();
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.power_state = "ON".into();
                        s.thresholds.mode = "eco".into();
                    }
                    json_resp("{\"ok\":true,\"mode\":\"eco\"}".into(), StatusCode(200))
                }
                (Method::Get, "/api/hold/clear") | (Method::Post, "/api/hold/clear") => {
                    if let Ok(mut h) = hold_http.lock() {
                        *h = None;
                    }
                    if let Ok(mut s) = snapshot_http.lock() {
                        s.hold_remaining_secs = 0;
                        s.hold_kind.clear();
                        s.block_reason.clear();
                        s.summary.clear();
                        if s.power_state.starts_with("USER_HOLD")
                            || s.power_state.starts_with("SOFT_PAUSE")
                            || s.power_state.contains("HOLD")
                        {
                            s.power_state = "ON".into();
                        }
                        let line = status_line_zh(
                            &s.thresholds.mode,
                            &s.ssid,
                            &s.band,
                            s.score,
                            &s.power_state,
                            "",
                        );
                        write_status_txt(&line);
                        update_module_description(&line);
                    }
                    log::info!("hold cleared via API");
                    json_resp(
                        "{\"ok\":true,\"hold_remaining_secs\":0,\"hold_kind\":\"\"}".into(),
                        StatusCode(200),
                    )
                }
                (Method::Get, "/api/soft-pause") | (Method::Post, "/api/soft-pause") => {
                    // ?mins=20 默认 20
                    let mins = url
                        .split('?')
                        .nth(1)
                        .and_then(|q| {
                            q.split('&').find_map(|p| {
                                let mut kv = p.splitn(2, '=');
                                match (kv.next(), kv.next()) {
                                    (Some("mins"), Some(v)) => v.parse::<u64>().ok(),
                                    _ => None,
                                }
                            })
                        })
                        .unwrap_or(20)
                        .clamp(1, 180);
                    let until = Instant::now() + Duration::from_secs(mins * 60);
                    *hold_http.lock().unwrap() = Some(HoldState {
                        until,
                        kind: HoldKind::SoftPause,
                    });
                    if let Ok(mut s) = snapshot_http.lock() {
                        let rem = mins * 60;
                        s.hold_remaining_secs = rem;
                        s.hold_kind = HoldKind::SoftPause.as_str().into();
                        s.power_state = HoldKind::SoftPause.power_state(rem);
                        s.block_reason = HoldKind::SoftPause.block_zh(rem);
                        s.summary = HoldKind::SoftPause.summary_zh(rem);
                        let line = status_line_zh(
                            &s.thresholds.mode,
                            &s.ssid,
                            &s.band,
                            s.score,
                            &s.power_state,
                            &s.block_reason,
                        );
                        write_status_txt(&line);
                        update_module_description(&line);
                    }
                    log::info!("soft-pause {mins} min via API");
                    json_resp(
                        format!(
                            "{{\"ok\":true,\"mins\":{mins},\"hold_remaining_secs\":{},\"hold_kind\":\"soft_pause\"}}",
                            mins * 60
                        ),
                        StatusCode(200),
                    )
                }
                (Method::Get, "/api/scan") | (Method::Post, "/api/scan") => {
                    let home = config_http.lock().unwrap().home_aps.clone();
                    let scan_res = (|| -> Result<Vec<crate::band_bond::ScanApView>, String> {
                        {
                            let w = wpa_http.lock().map_err(|e| e.to_string())?;
                            let _ = w.command("SCAN");
                        }
                        // 框架扫描：中文 SSID 更完整
                        let _ = std::process::Command::new("/system/bin/cmd")
                            .args(["wifi", "start-scan"])
                            .output();
                        thread::sleep(Duration::from_millis(1500));
                        let (wpa_aps, saved_ssids, cur_ssid) = {
                            let w = wpa_http.lock().map_err(|e| e.to_string())?;
                            let raw = w.scan_results().unwrap_or_default();
                            let aps = parse_scan_results(&raw);
                            let list_raw = w.list_networks().unwrap_or_default();
                            let wpa_ssids: Vec<String> = parse_list_networks(&list_raw)
                                .into_iter()
                                .map(|(_, s)| s)
                                .collect();
                            let saved = wifi_framework::merge_saved_ssids(&wpa_ssids);
                            let cur = w
                                .status()
                                .ok()
                                .and_then(|s| s.ssid)
                                .unwrap_or_default();
                            (aps, saved, cur)
                        };
                        let cmd_aps = {
                            let out = std::process::Command::new("/system/bin/cmd")
                                .args(["wifi", "list-scan-results"])
                                .output()
                                .ok();
                            out.and_then(|o| {
                                if o.status.success() {
                                    Some(parse_cmd_scan_results(&String::from_utf8_lossy(
                                        &o.stdout,
                                    )))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default()
                        };
                        let aps = merge_scan_aps(wpa_aps, cmd_aps);
                        // 双频候选仅限：系统已保存 + 当前连接 的 stem（邻居不标）
                        let mut stem_ssids = saved_ssids;
                        if !cur_ssid.is_empty() {
                            stem_ssids.push(cur_ssid);
                        }
                        let allow = stems_from_ssids(&stem_ssids);
                        Ok(scan_views_filtered(&aps, &home, Some(&allow)))
                    })();
                    match scan_res {
                        Ok(list) => {
                            let body = serde_json::json!({
                                "ok": true,
                                "count": list.len(),
                                "aps": list,
                                "home_ap_count": home.len(),
                            });
                            json_resp(body.to_string(), StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Get, "/api/init-config") | (Method::Post, "/api/init-config") => {
                    match Config::init_if_missing() {
                        Ok(written) => {
                            if written {
                                if let Ok(mut c) = config_http.lock() {
                                    let _ = c.reload();
                                    // 同步快照阈值
                                    if let Ok(mut s) = snapshot_http.lock() {
                                        s.thresholds = ThresholdsView {
                                            score_detect_threshold: c.score_detect_threshold,
                                            score_switch_threshold: c.score_switch_threshold,
                                            upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                            mode: c.mode.clone(),
                                        };
                                        s.user_hold_secs = c.user_hold_secs;
                                    }
                                }
                                log::info!("配置初始化完成（已落盘日用默认）");
                            }
                            let body = serde_json::json!({
                                "ok": true,
                                "initialized": written,
                                "persisted": Config::is_persisted(),
                            });
                            json_resp(body.to_string(), StatusCode(200))
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Post, "/api/config/reset") => {
                    // 热更新后配置重置：备份 → 删 config.toml → 回内存默认
                    let path = std::path::PathBuf::from("/data/adb/amberguard/config.toml");
                    if path.is_file() {
                        let ts = unix_now();
                        let bak = format!(
                            "/data/adb/amberguard/config.toml.bak.{ts}"
                        );
                        let _ = std::fs::copy(&path, &bak);
                        let _ = std::fs::remove_file(&path);
                    }
                    if let Ok(mut c) = config_http.lock() {
                        *c = Config::default();
                        let _ = c.save();
                        file_log::set_level(&c.log_level);
                        if let Ok(mut s) = snapshot_http.lock() {
                            s.thresholds = ThresholdsView {
                                score_detect_threshold: c.score_detect_threshold,
                                score_switch_threshold: c.score_switch_threshold,
                                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                                mode: c.mode.clone(),
                            };
                            s.user_hold_secs = c.user_hold_secs;
                            s.update_info = None;
                        }
                    }
                    let _ = std::fs::remove_file("/data/adb/amberguard/update.txt");
                    log::info!("config reset via API (hot update)");
                    json_resp(r#"{"ok":true,"reset":true}"#.into(), StatusCode(200))
                }
                (Method::Post, "/api/notify/test") => {
                    log::info!("api notify/test requested");
                    notify::test();
                    json_resp(r#"{"ok":true}"#.into(), StatusCode(200))
                }
                (Method::Post, "/api/connect") => {
                    let body = read_body(&mut req);
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                    let ssid = parsed.get("ssid").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let bssid = parsed.get("bssid").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if ssid.is_empty() {
                        log::warn!("api connect: empty ssid (bad request)");
                        json_resp(r#"{"ok":false,"error":"ssid is required"}"#.into(), StatusCode(400))
                    } else {
                        log::info!("api connect: handling ssid={ssid} bssid={bssid}");
                        let bssid_arg = if bssid.is_empty() { None } else { Some(bssid.as_str()) };
                        match wifi_framework::framework_connect(&ssid, bssid_arg) {
                            Ok(()) => {
                                log::info!("api connect: success ssid={ssid} bssid={bssid}");
                                let hold_secs = { config_http.lock().unwrap().user_hold_secs };
                                if hold_secs > 0 {
                                    *hold_http.lock().unwrap() = Some(HoldState {
                                        until: Instant::now() + Duration::from_secs(hold_secs),
                                        kind: HoldKind::Manual,
                                    });
                                }
                                json_resp(
                                    format!(r#"{{"ok":true,"ssid":"{ssid}","bssid":"{bssid}"}}"#),
                                    StatusCode(200),
                                )
                            }
                            Err(e) => {
                                log::warn!("api connect: failed ssid={ssid}: {e}");
                                json_resp(
                                    format!(r#"{{"ok":false,"error":"{e}"}}"#),
                                    StatusCode(500),
                                )
                            }
                        }
                    }
                }
                (Method::Get, "/api/logs") => {
                    // ?lines=200
                    let lines = url
                        .split('?')
                        .nth(1)
                        .and_then(|q| {
                            q.split('&').find_map(|p| {
                                let mut kv = p.splitn(2, '=');
                                match (kv.next(), kv.next()) {
                                    (Some("lines"), Some(v)) => v.parse::<usize>().ok(),
                                    _ => None,
                                }
                            })
                        })
                        .unwrap_or(200)
                        .clamp(20, 2000);
                    let body = file_log::tail(lines);
                    let json = serde_json::json!({
                        "ok": true,
                        "path": file_log::log_path_display(),
                        "lines": lines,
                        "content": body,
                    });
                    json_resp(json.to_string(), StatusCode(200))
                }
                (Method::Post, "/api/logs/clear") | (Method::Get, "/api/logs/clear") => {
                    match file_log::clear() {
                        Ok(()) => {
                            log::info!("log cleared via API");
                            json_resp(
                                "{\"ok\":true,\"cleared\":true}".into(),
                                StatusCode(200),
                            )
                        }
                        Err(e) => json_resp(
                            format!("{{\"ok\":false,\"error\":\"{e}\"}}"),
                            StatusCode(500),
                        ),
                    }
                }
                (Method::Get, "/") | (Method::Get, "/index.html") => {
                    let html = include_bytes!("web/static/index.html");
                    let mut r = Response::from_data(html.to_vec());
                    for kv in &[
                        (&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]),
                        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
                    ] {
                        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
                            r.add_header(h);
                        }
                    }
                    r
                }
                (Method::Options, _) => {
                    let mut r = Response::from_string("").with_status_code(StatusCode(204));
                    for kv in &[
                        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
                        (&b"Access-Control-Allow-Methods"[..], &b"GET, POST, PUT, OPTIONS"[..]),
                        (&b"Access-Control-Allow-Headers"[..], &b"Content-Type"[..]),
                    ] {
                        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
                            r.add_header(h);
                        }
                    }
                    r
                }
                _ => {
                    if path.starts_with("/api/") {
                        json_resp(
                            "{\"ok\":false,\"error\":\"not found\"}".into(),
                            StatusCode(404),
                        )
                    } else {
                        let html = include_bytes!("web/static/index.html");
                        Response::from_data(html.to_vec())
                    }
                }
            };
            let _ = req.respond(resp);
        }
    });

    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let mut prev_station = StationSample::default();
    // iw 采样节流：息屏不采；空闲 ≥3s；观察/防抖 1s
    let mut last_iw_at = Instant::now() - Duration::from_secs(10);
    let mut cached_retry: Option<f32> = None;
    let mut cached_tx_delta: u64 = 0;
    // 链路键 ssid|bssid；daemon 自切后短暂忽略变更（含失败尝试后的回弹）
    let mut prev_link_key = String::new();
    let mut suppress_link_change_until: Option<Instant> = None;
    // 切后短锁 BSSID，防 band-steering 踢回
    let mut lock_bssid_until: Option<Instant> = None;
    // 追优：更好同频 AP 持续可见 (bssid_lower, since)
    let mut roam_pending: Option<(String, Instant)> = None;
    let mut screen_on_grace_until: Option<Instant> = None;
    let mut last_screen = PowerState::On;
    // 启动时同步 mode
    let mut last_eco = {
        let c = config.lock().unwrap();
        sm.apply_eco(c.mode == "eco");
        c.mode == "eco"
    };
    {
        let c = config.lock().unwrap();
        if let Ok(mut s) = snapshot.lock() {
            if c.mode == "pause" {
                s.power_state = "PAUSE".into();
            }
            s.user_hold_secs = c.user_hold_secs;
            s.thresholds = ThresholdsView {
                score_detect_threshold: c.score_detect_threshold,
                score_switch_threshold: c.score_switch_threshold,
                upswitch_rssi_min_dbm: c.upswitch_rssi_min_dbm,
                mode: c.mode.clone(),
            };
        }
    }

    let mut weak_bad_since: Option<Instant> = None;
    let mut weak_disconnected = false;
    /// 最近扫描到的偏好侧最强 RSSI（供阈值对照）
    let mut cached_best_5g: Option<i32> = None;
    /// 同一目标连续失败次数 → 加长冷却，避免狂点把系统网「管理员停用」
    let mut fail_streak_key = String::new();
    let mut fail_streak: u32 = 0;
    let mut fail_backoff_until: Option<Instant> = None;
    /// 防重复：switch None 日志最多 30s 一条
    let mut last_no_target_log: Option<Instant> = None;
    let mut last_no_target_reason: String = String::new();

    /// wpa 连续失败退避（避免 WiFi 关闭时每秒刷屏重试）
    let mut wpa_fail_streak: u32 = 0;
    let mut last_wpa_reconnect: Option<Instant> = None;
    let mut last_wpa_fail_log: Option<Instant> = None;

    /// 模块热更新检测：启动时记录关键文件 mtime
    let mut start_mtimes = record_module_mtimes();
    let mut last_mtime_check = Instant::now();

    loop {
        let (
            paused,
            switch_th,
            detect_th,
            up_rssi,
            home_aps,
            iface,
            mode,
            hold_secs,
            weak_action,
            rssi_disc,
            weak_hold,
            auto_rec,
            l3_on,
            eco,
            preferred_is_5g,
            bssid_lock_secs,
            roam_enable,
            roam_margin_db,
            roam_hold_secs,
            notify_enable,
            notify_switch,
            notify_weak,
            notify_ongoing_secs,
        ) = {
            let c = config.lock().unwrap();
            (
                c.mode == "pause",
                c.score_switch_threshold,
                c.score_detect_threshold,
                c.upswitch_rssi_min_dbm,
                c.home_aps.clone(),
                c.interface.clone(),
                c.mode.clone(),
                c.user_hold_secs,
                c.weak_action.clone(),
                c.rssi_disconnect_dbm,
                c.weak_hold_secs,
                c.auto_reconnect,
                c.l3_probe_enable,
                c.mode == "eco",
                c.preferred_band == "5" || c.preferred_band.is_empty(),
                c.bssid_lock_secs,
                c.roam_enable,
                c.roam_margin_db,
                c.roam_hold_secs,
                c.notify_enable,
                c.notify_switch,
                c.notify_weak,
                c.notify_ongoing_secs,
            )
        };

        if eco != last_eco {
            sm.apply_eco(eco);
            last_eco = eco;
            log::info!("mode debounce: eco={eco}");
        }

        // —— 模块热更新检测（每 5s 一次 stat，零开销）——
        if last_mtime_check.elapsed() >= Duration::from_secs(5) {
            last_mtime_check = Instant::now();
            if binary_changed(&start_mtimes) {
                let (new_ver, reset_flag) = read_module_prop();
                let need_reboot = reboot_files_changed(&start_mtimes);
                let msg = if need_reboot && reset_flag {
                    format!("模块已更新到 {new_ver}：需重启手机生效，且本次更新需重置配置")
                } else if need_reboot {
                    format!("模块已更新到 {new_ver}：需重启手机才能完整生效")
                } else if reset_flag {
                    format!("模块已更新到 {new_ver}：本次更新需重置配置")
                } else {
                    format!("模块已更新到 {new_ver}，正在自动重启…")
                };
                log::info!("hot update detected: ver={new_ver} need_reboot={need_reboot} reset={reset_flag}");
                let info = crate::web::UpdateInfo {
                    detected: true,
                    new_version: new_ver.clone(),
                    need_reboot,
                    need_config_reset: reset_flag,
                    message: msg.clone(),
                };
                if let Ok(mut s) = snapshot.lock() {
                    s.update_info = Some(info);
                    s.summary = msg.clone();
                }
                // 可热更新 → 退出让 service.sh 重拉新二进制；需重启/重置则留在旧进程提示用户
                if !need_reboot && !reset_flag {
                    write_status_txt("AmberGuard · 模块已更新 · 自动重启中");
                    notify::event("AmberGuard 已热更新", &format!("已自动切换到 {new_ver}"));
                    std::process::exit(0);
                }
                let _ = std::fs::write("/data/adb/amberguard/update.txt", &msg);
                // 需重启/需重置：发系统通知，不开面板也能看到
                notify::event("AmberGuard 已更新", &msg);
                // 首检后刷新 binary 基线，止住每 5s 重复触发（Branch B 防通知堆叠）
                start_mtimes.binary = std::fs::metadata("/proc/self/exe")
                    .ok()
                    .and_then(|m| m.modified().ok());
            }
        }

        let screen = power.current_state();
        if screen == PowerState::On && last_screen == PowerState::Off {
            screen_on_grace_until = Some(Instant::now() + Duration::from_secs(3));
        }
        last_screen = screen;
        let screen_off = screen == PowerState::Off;
        let in_grace = screen_on_grace_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false);
        let lock_rem_secs = lock_bssid_until
            .map(|t| {
                if Instant::now() < t {
                    t.saturating_duration_since(Instant::now()).as_secs()
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let bssid_locked = lock_rem_secs > 0;
        // 扫描间隔：息屏最稀；观察带（分<观察线）更勤 —— 用户调「观察线」应能感到扫/反应变化
        let prev_score = snapshot
            .lock()
            .ok()
            .map(|s| s.score)
            .unwrap_or(50.0);
        let observing = prev_score < detect_th;
        let scan_gap: u64 = if screen_off {
            60
        } else if eco {
            if observing {
                12
            } else {
                30
            }
        } else if observing {
            8
        } else {
            15
        };

        // hold 剩余 + 种类
        let (hold_rem, hold_kind_now) = {
            let mut h = hold_state.lock().unwrap();
            match h.as_ref() {
                Some(st) if Instant::now() < st.until => {
                    let rem = st.until.saturating_duration_since(Instant::now()).as_secs();
                    (rem, Some(st.kind))
                }
                Some(_) => {
                    *h = None;
                    (0, None)
                }
                None => (0, None),
            }
        };

        // 快照侧暂停标记与 config 对齐
        if paused {
            if let Ok(mut s) = snapshot.lock() {
                s.power_state = "PAUSE".into();
            }
        }

        let st = {
            let w = wpa.lock().unwrap();
            w.status_with_signal().ok()
        };

        if let Some(ref st) = st {
            let rssi = st.signal_dbm.unwrap_or(-100);
            let ssid_now = st.ssid.clone().unwrap_or_default();
            let bssid_now = st.bssid.clone().unwrap_or_default();
            let link_key = format!(
                "{}|{}",
                ssid_now.to_lowercase(),
                bssid_now.to_lowercase()
            );

            // 检测用户手动切网（非 daemon 发起）
            // 同 SSID 换 BSSID = 框架 802.11k/v 漫游，不算手切
            let prev_ssid = prev_link_key.split('|').next().unwrap_or("");
            let ssid_changed = !prev_ssid.is_empty()
                && ssid_now.to_lowercase() != prev_ssid;
            if !prev_link_key.is_empty()
                && link_key != prev_link_key
                && ssid_changed
                && st.wpa_state == "COMPLETED"
                && !ssid_now.is_empty()
            {
                let our = suppress_link_change_until
                    .map(|t| Instant::now() < t)
                    .unwrap_or(false);
                if !our && hold_secs > 0 && !paused {
                    let until = Instant::now() + Duration::from_secs(hold_secs);
                    *hold_state.lock().unwrap() = Some(HoldState {
                        until,
                        kind: HoldKind::Manual,
                    });
                    sm.reset_soft();
                    log::info!(
                        "manual switch detected {} -> {} ; manual_hold {}s",
                        prev_link_key,
                        link_key,
                        hold_secs
                    );
                }
            }
            if st.wpa_state == "COMPLETED" && !link_key.ends_with('|') {
                prev_link_key = link_key;
            }

            // 采样矩阵（刀5）：息屏跳过 iw；亮屏空闲 3s；观察带/切换中 1s
            let (retry_opt, tx_delta) = if screen_off {
                (cached_retry, 0)
            } else {
                let iw_gap_secs: u64 = if observing
                    || matches!(
                        sm.state,
                        crate::state_machine::State::GradientDetect
                            | crate::state_machine::State::Switching
                    ) {
                    1
                } else {
                    3
                };
                if last_iw_at.elapsed() < Duration::from_secs(iw_gap_secs) {
                    (cached_retry, cached_tx_delta)
                } else {
                    last_iw_at = Instant::now();
                    match iw_station_dump(&iface) {
                        Ok(raw) => {
                            let cur = parse_iw_station(&raw);
                            let rate = retry_rate(&prev_station, &cur);
                            let delta = cur.tx_packets.saturating_sub(prev_station.tx_packets);
                            prev_station = cur;
                            cached_retry = rate;
                            cached_tx_delta = delta;
                            (rate, delta)
                        }
                        Err(e) => {
                            log::debug!("iw station dump: {e}");
                            (cached_retry, cached_tx_delta)
                        }
                    }
                }
            };

            let score = health_score(rssi, retry_opt, tx_delta, None);
            let band = match st.freq {
                Some(f) if f > 5000 => "5",
                Some(_) => "2.4",
                None => "?",
            };
            let on_preferred = if preferred_is_5g {
                band == "5"
            } else {
                band == "2.4"
            };

            let (hold_rem_now, hold_kind_now) = {
                let h = hold_state.lock().unwrap();
                match h.as_ref() {
                    Some(st) if Instant::now() < st.until => (
                        st.until
                            .saturating_duration_since(Instant::now())
                            .as_secs(),
                        Some(st.kind),
                    ),
                    _ => (0, None),
                }
            };

            let in_home_now = link_in_home(&home_aps, &bssid_now, &ssid_now);
            let pen_rem = sm.penalty_remaining_secs();

            let th_hint = threshold_hint_zh(
                on_preferred,
                score,
                rssi,
                switch_th,
                detect_th,
                up_rssi,
                cached_best_5g,
            );

            // 中文原因条（阻塞 > 阈值对照提示）
            let block_reason = if paused {
                "已暂停守护".to_string()
            } else if let Some(k) = hold_kind_now {
                k.block_zh(hold_rem_now)
            } else if pen_rem > 0 {
                format!("切换冷却中（剩 {pen_rem}s）")
            } else if screen_off {
                "息屏降频，不主动切网".to_string()
            } else if in_grace {
                "亮屏冷静窗，暂不切网".to_string()
            } else if bssid_locked {
                format!("短时锁定当前 AP（防踢回，剩 {lock_rem_secs}s）")
            } else if fail_backoff_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false)
            {
                let left = fail_backoff_until
                    .map(|t| t.saturating_duration_since(Instant::now()).as_secs())
                    .unwrap_or(0);
                format!("目标连失败多次，退避中（剩 {left}s，避免系统停用网络）")
            } else if !home_aps.is_empty() && !in_home_now {
                "当前不在家网".to_string()
            } else if weak_disconnected {
                "弱信号已断开".to_string()
            } else if !Config::is_persisted() {
                "配置未落盘（可用默认运行）".to_string()
            } else {
                // 无硬阻塞：原因条留空，阈值说明只走 threshold_hint（避免黄/灰双条重复）
                String::new()
            };

            {
                let mut s = snapshot.lock().unwrap();
                s.state = st.wpa_state.clone();
                s.rssi = rssi;
                s.ssid = ssid_now.clone();
                s.bssid = bssid_now.clone();
                s.band = band.into();
                s.score = score;
                s.hold_remaining_secs = hold_rem_now;
                s.hold_kind = hold_kind_now.map(|k| k.as_str().into()).unwrap_or_default();
                s.user_hold_secs = hold_secs;
                s.link_ctrl = "ok".into();
                s.home_ap_count = home_aps.len();
                s.in_home = in_home_now;
                s.block_reason = block_reason.clone();
                s.threshold_hint = th_hint.clone();
                s.best_5g_rssi = cached_best_5g;
                s.penalty_remaining_secs = pen_rem;
                s.bssid_lock_remaining_secs = lock_rem_secs;
                s.summary = if let Some(k) = hold_kind_now {
                    k.summary_zh(hold_rem_now)
                } else {
                    status_summary_zh(
                        band,
                        score,
                        &mode,
                        hold_rem_now,
                        pen_rem,
                        in_home_now,
                        home_aps.len(),
                        screen_off,
                        paused,
                        on_preferred,
                        lock_rem_secs,
                        preferred_is_5g,
                    )
                };
                s.screen = if screen_off { "OFF" } else { "ON" }.into();
                s.thresholds = ThresholdsView {
                    score_detect_threshold: detect_th,
                    score_switch_threshold: switch_th,
                    upswitch_rssi_min_dbm: up_rssi,
                    mode: mode.clone(),
                };
                if s.power_state != "PAUSE" {
                    if weak_disconnected {
                        s.power_state = "WEAK_OFF".into();
                    } else if let Some(k) = hold_kind_now {
                        s.power_state = k.power_state(hold_rem_now);
                    } else if !in_home_now && !home_aps.is_empty() {
                        s.power_state = "OUT_OF_HOME".into();
                    } else if screen_off {
                        s.power_state = "SCREEN_OFF".into();
                    } else {
                        s.power_state = format!("{:?}", sm.state);
                    }
                }
                // 面具列表 description + status.txt：中文实时模块状态
                let line = status_line_zh(
                    &mode,
                    &ssid_now,
                    band,
                    score,
                    &s.power_state,
                    &block_reason,
                );
                write_status_txt(&line);
                update_module_description(&line);
            }

            // 息屏：只更新状态，不 SCAN/不切换；睡 1s（勿 5s，否则亮屏后长时间「卡在息屏」）
            if screen_off {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            if paused {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 手动保护期内：只观测，不切网（手切优先，模块不抢）
            if hold_rem_now > 0 {
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 连续代连失败退避：不狂点，避免系统「管理员停用」
            if fail_backoff_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false)
            {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            if fail_backoff_until
                .map(|t| Instant::now() >= t)
                .unwrap_or(false)
            {
                fail_backoff_until = None;
            }

            // 亮屏冷静窗：全跳过
            if in_grace {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            // BSSID 短锁：跳过健康分调度；弱信号救援仍可（刀7）

            // 断后自动重连
            if weak_disconnected && auto_rec {
                if st.wpa_state != "COMPLETED" {
                    if last_scan.elapsed() > Duration::from_secs(10) {
                        log::info!("weak reconnect: RECONNECT");
                        let _ = wpa.lock().unwrap().command("RECONNECT");
                        last_scan = Instant::now();
                    }
                } else {
                    weak_disconnected = false;
                    if let Ok(mut snap) = snapshot.lock() {
                        if snap.last_error.starts_with("弱信号") {
                            snap.last_error.clear();
                        }
                    }
                }
            } else if st.wpa_state == "COMPLETED" {
                weak_disconnected = false;
            }

            // 已配置家网且当前不在家网：不自动切、不弱信号断
            if !home_aps.is_empty() && !in_home_now {
                weak_bad_since = None;
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            // 弱信号：先给切换机会，满时限仍差才考虑断（默认 off）
            // ponytail: OUT_OF_HOME/hold/pause 已在上方跳过
            let mut weak_rescue = false;
            if weak_action == "disconnect" && st.wpa_state == "COMPLETED" {
                if rssi < rssi_disc {
                    if weak_bad_since.is_none() {
                        weak_bad_since = Some(Instant::now());
                    }
                    let elapsed = weak_bad_since
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    // 满时限后每 ≥8s 救援一次，避免每秒 SCAN
                    if elapsed >= weak_hold && last_scan.elapsed() >= Duration::from_secs(8) {
                        weak_rescue = true;
                        log::info!(
                            "weak rescue window: rssi={rssi} < {rssi_disc} for {elapsed}s — try switch before disconnect"
                        );
                    }
                } else {
                    weak_bad_since = None;
                }
            } else {
                weak_bad_since = None;
            }

            let hint = sm.on_score(score, switch_th, detect_th, on_preferred);
            // 短锁期间：忽略 Score 上/下切与追优，仅弱救援可进
            let hint = if bssid_locked {
                SwitchHint::None
            } else {
                hint
            };
            let hold_now = hold_rem_now; // 上方已算
            // 追优探针：偏好频段 + 分<观察线 + 已配家网（默认=5G 内追更好 AP）
            let want_roam_probe = roam_enable
                && on_preferred
                && score < detect_th
                && !home_aps.is_empty()
                && hold_now == 0
                && !screen_off
                && !bssid_locked
                && !paused
                && hint == SwitchHint::None
                && !weak_rescue;

            if hint != SwitchHint::None || weak_rescue || want_roam_probe {
                if last_scan.elapsed() > Duration::from_secs(scan_gap) || weak_rescue {
                    let _ = wpa.lock().unwrap().command("SCAN");
                    let _ = std::process::Command::new("/system/bin/cmd")
                        .args(["wifi", "start-scan"])
                        .output();
                    thread::sleep(Duration::from_secs(2));
                    last_scan = Instant::now();
                }
                // wpa 结果常残缺；合并 cmd list-scan-results（家网 BSSID / 中文 SSID）
                let wpa_scans = {
                    let w = wpa.lock().unwrap();
                    w.scan_results()
                        .ok()
                        .map(|r| parse_scan_results(&r))
                        .unwrap_or_default()
                };
                let cmd_scans = std::process::Command::new("/system/bin/cmd")
                    .args(["wifi", "list-scan-results"])
                    .output()
                    .ok()
                    .map(|o| parse_cmd_scan_results(&String::from_utf8_lossy(&o.stdout)))
                    .unwrap_or_default();
                let scans = merge_scan_aps(wpa_scans, cmd_scans);
                // 更新家网 5G 最强（不论是否达标），供面板对照上切线
                // 偏好频段上最强家网 AP（上切对照）；字段名历史原因仍叫 best_5g
                cached_best_5g = scans
                    .iter()
                    .filter(|a| {
                        a.is_5g() == preferred_is_5g
                            && (home_aps.is_empty() || home_contains(&home_aps, &a.bssid))
                    })
                    .map(|a| a.signal)
                    .max();
                let ssid = ssid_now;
                let cur_bssid = bssid_now;
                let cur_is_5g = band == "5";

                let mut roam_fire = false;
                let mut target = match hint {
                    // 下切=离开偏好 → 非偏好频段；上切=回到偏好
                    SwitchHint::Downswitch => {
                        roam_pending = None;
                        // 同频优选（逃生）：≥+8 dB 先换同频；否则下切非偏好
                        best_on_band(
                            &scans,
                            &ssid,
                            preferred_is_5g,
                            rssi + 8,
                            &home_aps,
                        )
                        .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid))
                        .or_else(|| {
                            best_on_band(
                                &scans,
                                &ssid,
                                !preferred_is_5g,
                                -80,
                                &home_aps,
                            )
                        })
                    }
                    SwitchHint::Upswitch => {
                        roam_pending = None;
                        best_on_band(
                            &scans,
                            &ssid,
                            preferred_is_5g,
                            up_rssi,
                            &home_aps,
                        )
                    }
                    SwitchHint::SameBandRoam => None, // 由下方主循环填充
                    SwitchHint::None if weak_rescue => {
                        roam_pending = None;
                        // 救援：① 同频更强(+5dB) ② 否则 2.4 且明显高于断开阈值
                        let better_same = best_on_band(
                            &scans,
                            &ssid,
                            cur_is_5g,
                            rssi + 5,
                            &home_aps,
                        )
                        .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid));
                        better_same.or_else(|| {
                            if cur_is_5g {
                                best_on_band(
                                    &scans,
                                    &ssid,
                                    false,
                                    rssi_disc + 10,
                                    &home_aps,
                                )
                                .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid))
                            } else {
                                None
                            }
                        })
                    }
                    SwitchHint::None => None,
                };

                // 追优：仅同频更好家网 AP（不下 2.4）；dB 主判，分<观察线作门
                if target.is_none() && want_roam_probe {
                    let better = best_on_band(
                        &scans,
                        &ssid,
                        preferred_is_5g,
                        rssi + roam_margin_db,
                        &home_aps,
                    )
                    .filter(|a| !a.bssid.eq_ignore_ascii_case(&cur_bssid));
                    if let Some(peer) = better {
                        let key = peer.bssid.to_lowercase();
                        match &roam_pending {
                            Some((kb, since))
                                if *kb == key
                                    && since.elapsed() >= Duration::from_secs(roam_hold_secs)
                            => {
                                log::info!(
                                    "roam fire: {} {} dBm (cur {rssi}, margin +{roam_margin_db}, hold {roam_hold_secs}s)",
                                    peer.bssid,
                                    peer.signal
                                );
                                target = Some(peer);
                                roam_fire = true;
                                roam_pending = None;
                            }
                            Some((kb, _)) if *kb == key => {
                                // 持续可见，等待满 hold
                            }
                            _ => {
                                log::info!(
                                    "roam candidate: {} sig={} (need +{roam_margin_db} dB vs {rssi}, hold {roam_hold_secs}s)",
                                    peer.bssid,
                                    peer.signal
                                );
                                roam_pending = Some((key, Instant::now()));
                            }
                        }
                    } else {
                        roam_pending = None;
                    }
                } else if !want_roam_probe && hint == SwitchHint::None {
                    roam_pending = None;
                }

                if let Some(peer) = target {
                    if peer.bssid.eq_ignore_ascii_case(&cur_bssid) {
                        sm.finish_switch_ok();
                        if weak_rescue {
                            // 没有更好目标 → 断
                            log::warn!(
                                "weak disconnect: no better peer (still on {})",
                                cur_bssid
                            );
                            let _ = wpa.lock().unwrap().command("DISCONNECT");
                            weak_disconnected = true;
                            weak_bad_since = None;
                            if let Ok(mut snap) = snapshot.lock() {
                                snap.last_error = format!(
                                    "弱信号已断开（{rssi} dBm，无更优 AP）"
                                );
                                snap.power_state = "WEAK_OFF".into();
                            }
                        }
                        continue;
                    }
                    let bond_key = format!("{ssid}->{}/{}", peer.ssid, peer.bssid);
                    let same_ssid = peer.ssid == ssid;
                    let reason = if weak_rescue && hint == SwitchHint::None {
                        "WeakRescue"
                    } else if roam_fire {
                        "SameBandRoam"
                    } else {
                        "Score"
                    };
                    let hint_log = if roam_fire {
                        SwitchHint::SameBandRoam
                    } else {
                        hint
                    };
                    let from_band = band.to_string();
                    let to_band = if peer.freq > 5000 { "5" } else { "2.4" }.to_string();
                    let switch_t0 = Instant::now();
                    log::info!(
                        "switch {reason}/{:?}: {} {} ssid={} freq={} sig={} score={score:.1}",
                        hint_log,
                        if same_ssid { "ROAM" } else { "SELECT" },
                        peer.bssid,
                        peer.ssid,
                        peer.freq,
                        peer.signal
                    );

                    // 标记：随后链路变化视为 daemon 自切
                    suppress_link_change_until =
                        Some(Instant::now() + Duration::from_secs(12));

                    // 尝试前尽量 enable 所有 wpa 网络，减轻「管理员停用」残留
                    {
                        let _ = wpa.lock().unwrap().command("ENABLE_NETWORK all");
                    }
                    let mut selected_nid: Option<u32> = None;
                    let switch_res = if same_ssid {
                        match wpa.lock().unwrap().roam(&peer.bssid) {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                log::info!("ROAM fail ({e}), try framework connect");
                                wifi_framework::framework_connect(
                                    &peer.ssid,
                                    Some(&peer.bssid),
                                )
                                .map_err(wpa_ctrl::WpaError::Parse)
                            }
                        }
                    } else {
                        match wifi_framework::framework_connect(
                            &peer.ssid,
                            Some(&peer.bssid),
                        ) {
                            Ok(()) => {
                                log::info!("framework connect issued -> {}", peer.ssid);
                                Ok(())
                            }
                            Err(e_fw) => {
                                log::warn!("framework connect: {e_fw}; fallback wpa SELECT");
                                let list =
                                    wpa.lock().unwrap().list_networks().unwrap_or_default();
                                match network_id_for_ssid(&list, &peer.ssid) {
                                    Some(nid) => {
                                        selected_nid = Some(nid);
                                        let w = wpa.lock().unwrap();
                                        let _ = w.enable_network(nid);
                                        // 异名双频不钉 BSSID，避免选错/失败
                                        let _ = w.set_network_bssid(nid, "\"\"");
                                        w.select_network(nid)
                                    }
                                    None => {
                                        let msg = format!(
                                            "无法切换到「{}」：{e_fw}；请确认系统已保存该网且未被停用",
                                            peer.ssid
                                        );
                                        log::warn!("{msg}");
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.last_error = msg.clone();
                                            snap.block_reason = "对侧网络无法选中（或已停用）".into();
                                        }
                                        Err(wpa_ctrl::WpaError::Parse(msg))
                                    }
                                }
                            }
                        }
                    };

                    match switch_res {
                        Ok(()) => {
                            // 异名：只验 SSID；同名漫游：验 BSSID
                            let mut ok = false;
                            let mut last_got = String::new();
                            for i in 0..48 {
                                thread::sleep(Duration::from_millis(250));
                                if let Ok(w) = wpa.lock() {
                                    if let Ok(s2) = w.status() {
                                        let gs = s2.ssid.clone().unwrap_or_default();
                                        let gb = s2.bssid.clone().unwrap_or_default();
                                        last_got = format!(
                                            "{}|{}|{}",
                                            s2.wpa_state, gs, gb
                                        );
                                        if link_reached_peer(
                                            &s2,
                                            &peer.ssid,
                                            &peer.bssid,
                                            same_ssid,
                                        ) {
                                            ok = true;
                                            log::info!(
                                                "switch verified at {}ms -> {} {}",
                                                (i + 1) * 250,
                                                gs,
                                                gb
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                            // 未落地且异名：再试一次不带 BSSID 的框架连接（部分机 -b 空转）
                            if !ok && !same_ssid {
                                log::info!("retry framework connect without bssid -> {}", peer.ssid);
                                if wifi_framework::framework_connect(&peer.ssid, None).is_ok() {
                                    for i in 0..32 {
                                        thread::sleep(Duration::from_millis(250));
                                        if let Ok(w) = wpa.lock() {
                                            if let Ok(s2) = w.status() {
                                                last_got = format!(
                                                    "{}|{}|{}",
                                                    s2.wpa_state,
                                                    s2.ssid.clone().unwrap_or_default(),
                                                    s2.bssid.clone().unwrap_or_default()
                                                );
                                                if link_reached_peer(
                                                    &s2,
                                                    &peer.ssid,
                                                    &peer.bssid,
                                                    false,
                                                ) {
                                                    ok = true;
                                                    log::info!(
                                                        "switch verified (retry) at {}ms",
                                                        (i + 1) * 250
                                                    );
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // 清目标网 bssid 钉（无论成败，避免长期锁死）
                            if let Some(nid) = selected_nid {
                                if let Ok(w) = wpa.lock() {
                                    let _ = w.set_network_bssid(nid, "\"\"");
                                }
                            }
                            let mut result = if ok { "Ok" } else { "Fail" }.to_string();
                            if !ok {
                                log::warn!(
                                    "switch not landed: want {}/{} got {last_got}",
                                    peer.ssid,
                                    peer.bssid
                                );
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error = format!(
                                        "切换未生效（目标 {}，仍为 {last_got}）",
                                        peer.ssid
                                    );
                                }
                            }
                            // L3：链路已落到目标后只作连通性标注，失败不撤销成功、不进惩罚
                            // （国内 gstatic DNS 失败曾导致「已切上却 L3Timeout+冷却」）
                            if ok && l3_on {
                                match l3_probe(Duration::from_secs(2)) {
                                    Ok(k) => {
                                        result = k.hist_result().into();
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = k.as_prefix().into();
                                        }
                                        log::info!("L3 probe {}", k.as_prefix());
                                    }
                                    Err((k, e)) => {
                                        log::warn!(
                                            "L3 probe soft-fail {} (仍计切换成功): {e}",
                                            k.as_prefix()
                                        );
                                        result = k.hist_result().into();
                                        if let Ok(mut snap) = snapshot.lock() {
                                            snap.l3_last = format!("{}:{e}", k.as_prefix());
                                        }
                                    }
                                }
                            } else if ok {
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.l3_last = L3Kind::Skip.as_prefix().into();
                                }
                            }
                            let dur = switch_t0.elapsed().as_millis() as u64;
                            push_history(
                                &history,
                                SwitchEvent {
                                    ts_unix: unix_now(),
                                    from_ssid: ssid.clone(),
                                    to_ssid: peer.ssid.clone(),
                                    from_band: from_band.clone(),
                                    to_band: to_band.clone(),
                                    reason: reason.into(),
                                    result: result.clone(),
                                    duration_ms: dur,
                                },
                            );
                            // 无论成败：短时忽略链路键变化，避免「代连失败回弹」被当成手动切网
                            suppress_link_change_until =
                                Some(Instant::now() + Duration::from_secs(15));
                            if let Ok(w) = wpa.lock() {
                                if let Ok(s2) = w.status() {
                                    let ns = s2.ssid.clone().unwrap_or_default();
                                    let nb = s2.bssid.clone().unwrap_or_default();
                                    if !ns.is_empty() {
                                        prev_link_key = format!(
                                            "{}|{}",
                                            ns.to_lowercase(),
                                            nb.to_lowercase()
                                        );
                                    }
                                }
                            }
                            if ok {
                                sm.finish_switch_ok();
                                fail_streak = 0;
                                fail_streak_key.clear();
                                fail_backoff_until = None;
                                if bssid_lock_secs > 0 {
                                    lock_bssid_until = Some(
                                        Instant::now() + Duration::from_secs(bssid_lock_secs),
                                    );
                                }
                                if weak_rescue {
                                    weak_bad_since = None;
                                }
                                if let Ok(mut snap) = snapshot.lock() {
                                    if result.starts_with("Ok") {
                                        snap.last_error.clear();
                                    }
                                }
                                log::info!("switch OK -> {} ({})", peer.ssid, result);
                                last_no_target_reason.clear();
                                if notify_enable && notify_switch {
                                    notify::event_id(
                                    "AmberGuard",
                                    &format!("已切换到 {}G：{}", peer.band, peer.ssid),
                                    "amber_switch",
                                );
                                }
                            } else {
                                if fail_streak_key == bond_key {
                                    fail_streak = fail_streak.saturating_add(1);
                                } else {
                                    fail_streak_key = bond_key.clone();
                                    fail_streak = 1;
                                }
                                // 连续失败 ≥3：退避 3～5 分钟，别把网打停用
                                if fail_streak >= 3 {
                                    let back = 180 + (fail_streak as u64 - 3) * 60;
                                    let back = back.min(600);
                                    fail_backoff_until =
                                        Some(Instant::now() + Duration::from_secs(back));
                                    log::warn!(
                                        "switch fail streak={fail_streak} target={bond_key} → backoff {back}s"
                                    );
                                    if let Ok(mut snap) = snapshot.lock() {
                                        snap.last_error = format!(
                                            "连「{}」连续失败 {fail_streak} 次，已退避 {back}s。请系统里检查该网是否被停用后重连保存",
                                            peer.ssid
                                        );
                                    }
                                }
                                sm.enter_penalty(&bond_key);
                                if weak_rescue {
                                    log::warn!("weak disconnect after failed rescue switch");
                                    let _ = wpa.lock().unwrap().command("DISCONNECT");
                                    weak_disconnected = true;
                                    weak_bad_since = None;
                                    if let Ok(mut snap) = snapshot.lock() {
                                        snap.last_error =
                                            format!("弱信号已断开（切换失败，{rssi} dBm）");
                                        snap.power_state = "WEAK_OFF".into();
                                    }
                                    if notify_enable && notify_weak {
                                        notify::event("AmberGuard", &format!("弱信号已断开（{}dBm，救援切换未成功）", rssi));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("switch failed: {e}");
                            let msg = e.to_string();
                            if let Some(nid) = selected_nid {
                                if let Ok(w) = wpa.lock() {
                                    let _ = w.set_network_bssid(nid, "\"\"");
                                }
                            }
                            push_history(
                                &history,
                                SwitchEvent {
                                    ts_unix: unix_now(),
                                    from_ssid: ssid.clone(),
                                    to_ssid: peer.ssid.clone(),
                                    from_band: from_band.clone(),
                                    to_band: to_band.clone(),
                                    reason: reason.into(),
                                    result: "Fail".into(),
                                    duration_ms: switch_t0.elapsed().as_millis() as u64,
                                },
                            );
                            sm.enter_penalty(&bond_key);
                            if msg.contains("请先在系统设置") || msg.contains("no network id") {
                                if let Some(p) = sm.penalty.as_mut() {
                                    p.cooldown_secs = 15;
                                    p.until = Instant::now() + Duration::from_secs(15);
                                }
                            }
                            if weak_rescue {
                                log::warn!("weak disconnect after rescue error: {msg}");
                                let _ = wpa.lock().unwrap().command("DISCONNECT");
                                weak_disconnected = true;
                                weak_bad_since = None;
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error =
                                        format!("弱信号已断开（无可用网络，{rssi} dBm）");
                                    snap.power_state = "WEAK_OFF".into();
                                }
                                if notify_enable && notify_weak {
                                    notify::event("AmberGuard", &format!("弱信号已断开（{}dBm，无更优 AP）", rssi));
                                }
                            }
                        }
                    }
                } else {
                    if weak_rescue {
                        log::warn!(
                            "weak disconnect: no better peer in set (ssid={ssid}, home={})",
                            home_aps.len()
                        );
                        let _ = wpa.lock().unwrap().command("DISCONNECT");
                        weak_disconnected = true;
                        weak_bad_since = None;
                        if let Ok(mut snap) = snapshot.lock() {
                            snap.last_error =
                                format!("弱信号已断开（无更优 AP，{rssi} dBm < {rssi_disc}）");
                            snap.power_state = "WEAK_OFF".into();
                        }
                    } else {
                        // 非上切/下切时（None / SameBandRoam 等待中）：不写 last_error
                        let is_no_target = hint != SwitchHint::Upswitch
                            && hint != SwitchHint::Downswitch;
                        let do_log = if is_no_target {
                            // 只在原因变化时打 1 条，不再每 30s 重复
                            let why_preview = format!("无可用对端 AP（家网 {home} 个）", home = home_aps.len());
                            if why_preview != last_no_target_reason {
                                last_no_target_reason = why_preview;
                                true
                            } else {
                                false
                            }
                        } else {
                            true
                        };
                        if do_log {
                            let why = match hint {
                                SwitchHint::Upswitch => match cached_best_5g {
                                    Some(b) => format!(
                                        "上切未执行：家网 5G 最强 {b} dBm < 上切线 {up_rssi} dBm"
                                    ),
                                    None => format!(
                                        "上切未执行：未扫到家网 5G（上切线 {up_rssi} dBm）"
                                    ),
                                },
                                SwitchHint::Downswitch => {
                                    "下切未执行：未扫到可用 2.4G 家网 AP".into()
                                }
                                _ => format!("无可用对端 AP（家网 {home} 个）", home = home_aps.len()),
                            };
                            log::info!("switch {:?}: {why}", hint);
                            if !is_no_target {
                                // 上切/下切失败才写 last_error
                                if let Ok(mut snap) = snapshot.lock() {
                                    snap.last_error = why.clone();
                                    snap.block_reason = why;
                                    snap.best_5g_rssi = cached_best_5g;
                                    snap.threshold_hint = threshold_hint_zh(
                                        on_preferred,
                                        score,
                                        rssi,
                                        switch_th,
                                        detect_th,
                                        up_rssi,
                                        cached_best_5g,
                                    );
                                }
                            }
                        }
                        sm.finish_switch_ok();
                    }
                }
            }
        } else {
            // wpa STATUS 失败：仍必须刷新 hold/息屏文案，否则会永远卡在「保护中 30s」
            wpa_fail_streak = wpa_fail_streak.saturating_add(1);
            let now = Instant::now();
            let log_this = last_wpa_fail_log
                .map(|t| now.duration_since(t).as_secs() >= 30)
                .unwrap_or(true);
            if log_this {
                log::warn!("wpa status failed (streak={wpa_fail_streak}) — refresh snapshot + try reconnect");
                last_wpa_fail_log = Some(now);
            }
            {
                let mut s = snapshot.lock().unwrap();
                s.hold_remaining_secs = hold_rem;
                s.hold_kind = hold_kind_now
                    .map(|k| k.as_str().into())
                    .unwrap_or_default();
                s.user_hold_secs = hold_secs;
                s.home_ap_count = home_aps.len();
                s.link_ctrl = "reconnect".into();
                s.screen = if screen_off { "OFF" } else { "ON" }.into();
                s.block_reason = if paused {
                    "已暂停守护".into()
                } else if let Some(k) = hold_kind_now {
                    k.block_zh(hold_rem)
                } else if screen_off {
                    "息屏降频，不主动切网".into()
                } else {
                    "链路控制异常，正在重连 wpa".into()
                };
                s.summary = if let Some(k) = hold_kind_now {
                    k.summary_zh(hold_rem)
                } else if screen_off {
                    "息屏降频".into()
                } else {
                    "链路控制 · 重连中".into()
                };
                if let Some(k) = hold_kind_now {
                    s.power_state = k.power_state(hold_rem);
                } else if paused {
                    s.power_state = "PAUSE".into();
                } else if screen_off {
                    s.power_state = "SCREEN_OFF".into();
                } else if s.power_state.starts_with("USER_HOLD")
                    || s.power_state.starts_with("SOFT_PAUSE")
                {
                    s.power_state = "ON".into();
                }
                let line = status_line_zh(
                    &mode,
                    &s.ssid,
                    &s.band,
                    s.score,
                    &s.power_state,
                    &s.block_reason,
                );
                write_status_txt(&line);
                update_module_description(&line);
            }
            // 僵尸 ctrl socket：重连 wpa（连续失败时退避到 15s，不每秒刷屏）
            let should_reconnect = last_wpa_reconnect
                .map(|t| now.duration_since(t).as_secs() >= 15)
                .unwrap_or(true);
            if should_reconnect {
                last_wpa_reconnect = Some(now);
                match WpaCtrl::auto_connect() {
                    Ok(w) => {
                        log::info!("wpa reconnected after {} failures", wpa_fail_streak);
                        wpa_fail_streak = 0;
                        if let Ok(mut g) = wpa.lock() {
                            *g = w;
                        }
                        if let Ok(mut s) = snapshot.lock() {
                            s.link_ctrl = "ok".into();
                        }
                    }
                    Err(e) => {
                        log::warn!("wpa reconnect failed (streak={wpa_fail_streak}): {e}");
                        if let Ok(mut s) = snapshot.lock() {
                            s.link_ctrl = "fail".into();
                            if s.block_reason.is_empty() || s.block_reason.contains("重连") {
                                s.block_reason = format!("链路控制重连失败：{e}");
                            }
                        }
                    }
                }
            }
        }

        // 常驻状态条（亮屏时更新，息屏时清掉省电）
        if notify_enable && notify_ongoing_secs > 0 {
            if screen_off {
                notify::cancel_ongoing();
            } else if let Ok(s) = snapshot.lock() {
                let text = if !s.summary.is_empty() {
                    s.summary.clone()
                } else if s.state == "COMPLETED" {
                    format!("{} · {}dBm · 分{}", s.ssid, s.rssi, s.score as i32)
                } else {
                    "未连接".into()
                };
                notify::ongoing(&text, notify_ongoing_secs);
            }
        } else {
            notify::cancel_ongoing();
        }

        thread::sleep(Duration::from_secs(1));
    }
}

fn offline_loop() -> ! {
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::new()));
    let snapshot2 = Arc::clone(&snapshot);
    let addr: SocketAddr = "127.0.0.1:8080".parse().expect("addr");
    if let Ok(server) = Server::http(addr) {
        thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                let method = req.method().clone();
                let path = url.split('?').next().unwrap_or(&url);
                let resp = if method == Method::Get && (path == "/" || path == "/index.html") {
                    let html = include_bytes!("web/static/index.html");
                    let mut r = Response::from_data(html.to_vec());
                    for kv in &[
                        (&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]),
                        (&b"Access-Control-Allow-Origin"[..], &b"*"[..]),
                    ] {
                        if let Ok(h) = Header::from_bytes(kv.0, kv.1) {
                            r.add_header(h);
                        }
                    }
                    r
                } else {
                    let s = snapshot2.lock().unwrap();
                    let json = s.to_json();
                    let mut r = Response::from_string(json);
                    if let Ok(h) = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]) {
                        r.add_header(h);
                    }
                    r
                };
                let _ = req.respond(resp);
            }
        });
    }
    let mut c = 0u32;
    let mut last_wpa_retry = Instant::now();
    loop {
        // 周期重试 wpa：成功则 exit(0) 让 service.sh 重拉进主循环
        if last_wpa_retry.elapsed() >= Duration::from_secs(15) {
            last_wpa_retry = Instant::now();
            if let Ok(w) = WpaCtrl::auto_connect() {
                log::info!("wpa reconnected in offline_loop, restarting to enter main loop");
                drop(w);
                std::process::exit(0);
            }
        }
        if let Ok(mut s) = snapshot.lock() {
            s.rssi = -55 - (c % 20) as i32;
            s.score = 42.0;
            s.state = "OFFLINE".into();
            s.band = if c % 2 == 0 { "2.4" } else { "5" }.into();
        }
        c += 1;
        thread::sleep(Duration::from_secs(1));
    }
}
